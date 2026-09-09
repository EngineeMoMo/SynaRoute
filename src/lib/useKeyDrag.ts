// Key 卡片列表的鼠标拖放状态机（Pointer Events，零新增依赖）。
//
// # 为什么手写而不引 dnd-kit
//
// 需求很窄：**单列纵向、只在一个列表内、只用鼠标**。dnd-kit 要引 2~3 个包并在 Tauri
// WebView2 里另做一轮真机验证，而本仓的既有纪律是「本就在依赖树里才用」。
// 也**不用 HTML5 `draggable`/`dataTransfer`**：那条路在 WebView 里的 ghost image、
// 自动滚动与取消行为都更难控，且 KeyCard 里原本的注释就记着「拖拽在 Tauri WebView 里不稳」
// —— 那句话针对的正是 HTML5 DnD。Pointer Events + `setPointerCapture` 没有那些问题。
//
// # 三条刻意设计
//
// 1. **只有把手能起拖**（`onHandleDown` 挂在把手上）。整卡可拖会让卡片里的开关 / 编辑 /
//    删除 / 余额刷新 / 可点地址全部变成「点一下就开始拖」。
// 2. **拖动期间只更新预览，不写 store、不发 IPC**。每次 pointermove 都提交等于把一次
//    拖动变成几十次落盘（后端 `key_order` 模块头记的正是这条）。
// 3. **超过阈值才算拖动**。不设阈值时「点一下把手」会被当成一次零距离拖放，
//    触发一次无意义的 reorder 调用。

import { useCallback, useEffect, useRef, useState } from "react";
import { useStore } from "@/store";
import { dropAnchorAt, type CardBox } from "@/lib/keyOrdering";

/** 判定为「开始拖动」的最小位移（px）。低于它算点击。 */
const DRAG_THRESHOLD_PX = 4;

/**
 * 距滚动容器上/下边缘多少像素内开始自动滚动。
 *
 * 🔴 **没有自动滚动，这个功能对长列表基本不成立**：容器是 `overflow-y-auto`，
 * 而用户的真实配置有十几条 Key（docs 里记着 17 条）。把第 15 条拖到首位时，
 * 首位那张卡在视口外 —— 指针顶到容器上沿就再也走不动了，而「拖远」恰恰是
 * 用户要这个功能的全部理由（原话「一个一个点太麻烦了」）。
 */
const EDGE_PX = 48;
/** 每帧滚动步长。刻意小：大步长会让列表在指针下猛冲，看不清落点。 */
const SCROLL_STEP_PX = 10;

export interface KeyDragState {
  /** 正在被拖动的 Key（null = 没有拖动进行中） */
  draggingId: string | null;
  /**
   * 松手会插到谁**之前**。
   *
   * - `string`：插到该 Key 之前 → 那张卡上沿画指示线；
   * - `null`：插到**末尾** → 列表末尾画指示线；
   * - `undefined`：当前没有拖动。
   *
   * 三态刻意分开：把「末尾」也表示成 `undefined` 的话，界面无法区分
   * 「拖到最后」与「没在拖」，于是拖到末尾时一条指示线都不出现。
   */
  anchorId: string | null | undefined;
  onHandleDown: (keyId: string, e: React.PointerEvent) => void;
  registerCard: (keyId: string, el: HTMLElement | null) => void;
  /**
   * 挂到**滚动容器**上（列表那个 `overflow-y-auto` 的 div）。
   *
   * 自动滚动必须知道容器是谁。不挂的话拖到视口外就走不动了 —— 见 `EDGE_PX`。
   */
  scrollRef: (el: HTMLElement | null) => void;
}

/**
 * @param orderedIds 当前列表的**视觉顺序**（自上而下），与渲染顺序一致。
 */
export function useKeyDrag(orderedIds: string[]): KeyDragState {
  const reorderKey = useStore((s) => s.reorderKey);
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [anchorId, setAnchorId] = useState<string | null | undefined>(undefined);

  // 卡片 DOM 的登记表。用 ref 而非 state：它每次渲染都会被重新赋值，
  // 放进 state 会造成无限渲染循环。
  const cardsRef = useRef(new Map<string, HTMLElement>());
  // 拖动过程中的可变量放 ref：它们每帧都变，进 state 会触发几十次重渲染，
  // 而真正需要上屏的只有 draggingId / anchorId 两项。
  const startYRef = useRef(0);
  const activeRef = useRef(false);
  const anchorRef = useRef<string | null | undefined>(undefined);
  // 最新的视觉顺序。`onHandleDown` 只创建一次（memo 依赖为空），若直接闭包捕获
  // `orderedIds`，拖第二次时用的还是首次渲染那份顺序 —— 与本仓
  // 「async 回调里比对 prop 必须读 ref」那条同源。
  const idsRef = useRef(orderedIds);
  idsRef.current = orderedIds;
  // 滚动容器与自动滚动的 rAF 句柄。
  const scrollElRef = useRef<HTMLElement | null>(null);
  const rafRef = useRef<number | null>(null);
  const pointerYRef = useRef(0);

  const registerCard = useCallback((keyId: string, el: HTMLElement | null) => {
    if (el) cardsRef.current.set(keyId, el);
    else cardsRef.current.delete(keyId);
  }, []);

  const scrollRef = useCallback((el: HTMLElement | null) => {
    scrollElRef.current = el;
  }, []);

  /** 停掉自动滚动。**必须与启动点成对**，漏掉的话 rAF 会一直跑下去（列表自己滚）。 */
  const stopAutoScroll = useCallback(() => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  /** 结束拖动：清掉全部瞬态。取消与提交都要走它，漏一处就会卡在拖动态。 */
  const reset = useCallback(() => {
    stopAutoScroll();
    activeRef.current = false;
    anchorRef.current = undefined;
    setDraggingId(null);
    setAnchorId(undefined);
  }, [stopAutoScroll]);

  const onHandleDown = useCallback(
    (keyId: string, e: React.PointerEvent) => {
      // 只响应鼠标左键 / 触摸主指针。右键与中键不该起拖（右键要留给上下文菜单）。
      if (e.button !== 0) return;
      e.preventDefault();
      const handle = e.currentTarget as HTMLElement;
      // `setPointerCapture`：指针移出把手（必然发生，卡片会被拖离原位）之后仍然收得到
      // move/up 事件。没有它，快速拖动时 pointerup 会丢在别的元素上 → 卡在拖动态。
      try {
        handle.setPointerCapture(e.pointerId);
      } catch {
        // 某些环境（旧 WebView、测试桩）不支持 capture；退化成 window 监听仍可用。
      }
      startYRef.current = e.clientY;
      activeRef.current = false;

      const boxes = (): CardBox[] =>
        idsRef.current.flatMap((id) => {
          const el = cardsRef.current.get(id);
          if (!el) return [];
          const r = el.getBoundingClientRect();
          return [{ id, top: r.top, bottom: r.bottom }];
        });

      const onMove = (ev: PointerEvent) => {
        if (ev.pointerId !== e.pointerId) return;
        if (!activeRef.current) {
          if (Math.abs(ev.clientY - startYRef.current) < DRAG_THRESHOLD_PX) return;
          activeRef.current = true;
          setDraggingId(keyId);
        }
        pointerYRef.current = ev.clientY;
        // 每次都重算 rect：列表在拖动期间可能滚动（用户滚轮 / 触控板 / 下面的自动滚动）。
        const next = dropAnchorAt(boxes(), keyId, ev.clientY) ?? null;
        if (next !== anchorRef.current) {
          anchorRef.current = next;
          setAnchorId(next);
        }
        maybeAutoScroll();
      };

      /**
       * 指针贴住容器上/下边缘时，按帧滚动列表并**重算锚点**。
       *
       * 重算那一步不能省：滚动时指针本身不动（不再有 pointermove），若只滚不算，
       * 插入线会一直停在滚动开始时那张卡上 —— 松手落点与用户看到的线不一致。
       */
      function maybeAutoScroll() {
        const el = scrollElRef.current;
        if (!el || !activeRef.current) return stopAutoScroll();
        const r = el.getBoundingClientRect();
        const y = pointerYRef.current;
        let dir = 0;
        if (y < r.top + EDGE_PX) dir = -1;
        else if (y > r.bottom - EDGE_PX) dir = 1;
        if (dir === 0) return stopAutoScroll();
        if (rafRef.current !== null) return; // 已在滚
        const tick = () => {
          rafRef.current = null;
          if (!activeRef.current) return;
          const before = el.scrollTop;
          el.scrollTop = before + dir * SCROLL_STEP_PX;
          // 到顶/到底了就停：继续 rAF 只是白烧帧。
          if (el.scrollTop === before) return;
          const next = dropAnchorAt(boxes(), keyId, pointerYRef.current) ?? null;
          if (next !== anchorRef.current) {
            anchorRef.current = next;
            setAnchorId(next);
          }
          maybeAutoScroll();
        };
        rafRef.current = requestAnimationFrame(tick);
      }

      const onUp = (ev: PointerEvent) => {
        if (ev.pointerId !== e.pointerId) return;
        cleanup();
        const wasDragging = activeRef.current;
        const anchor = anchorRef.current;
        reset();
        // 没越过阈值 = 用户只是点了一下把手，什么都不做（别发一次空 reorder）。
        if (!wasDragging) return;
        // `null` 锚点 = 放末尾，对应 IPC 的 `beforeKeyId === undefined`。
        void reorderKey(keyId, anchor ?? undefined);
      };

      /** Esc / 窗口失焦 / 指针被系统取消 → 放弃这次拖动，不提交。 */
      const onCancel = () => {
        cleanup();
        reset();
      };
      const onKey = (ev: KeyboardEvent) => {
        if (ev.key === "Escape") onCancel();
      };

      function cleanup() {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onCancel);
        window.removeEventListener("blur", onCancel);
        window.removeEventListener("keydown", onKey);
        try {
          handle.releasePointerCapture(e.pointerId);
        } catch {
          // 已经释放过（pointercancel 会自动释放），忽略。
        }
      }

      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onCancel);
      window.addEventListener("blur", onCancel);
      window.addEventListener("keydown", onKey);
    },
    [reorderKey, reset, stopAutoScroll],
  );

  // 拖动中给 <body> 加 `select-none`：不加的话拖过卡片文字会选中一大片蓝底，
  // 看起来像界面出错了。
  useEffect(() => {
    if (!draggingId) return;
    document.body.classList.add("select-none");
    return () => document.body.classList.remove("select-none");
  }, [draggingId]);

  // 组件卸载时兜底停掉 rAF：切页面/切分类可能在拖动进行中发生，
  // 而那时 pointerup 永远不会到（DOM 已经没了）。
  useEffect(() => stopAutoScroll, [stopAutoScroll]);

  return { draggingId, anchorId, onHandleDown, registerCard, scrollRef };
}
