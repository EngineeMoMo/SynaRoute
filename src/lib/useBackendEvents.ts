import { useEffect, useRef } from "react";
import { isTauri } from "@/lib/bridge";
import { useStore } from "@/store";
import { usePolling } from "@/lib/usePolling";

/** 与后端 `events.rs` 的 `EVENT_NAME` 必须完全一致 */
const EVENT_NAME = "synaroute://state-changed";

/**
 * 兜底轮询周期（UX#5）。
 *
 * 有了后端推送之后轮询降级为**兜底**：推送队列满时会丢、事件监听注册前的几十毫秒窗口
 * 会漏、以及外部文件被别人改写（cc-switch 接管）这类后端根本不知道的变化。
 * 30s 是「用户察觉不到、但不会一直错下去」的量级。
 *
 * 唯一事实来源，各页面**禁止**再写 5000/2000 这类字面量 —— 那样调一次周期要改七处。
 */
export const FALLBACK_POLL_MS = 30_000;

export type BackendTopic = "config" | "settings" | "health" | "proxy" | "vault" | "logs";

interface StateChangedPayload {
  topic: BackendTopic;
  categoryId: string | null;
}

/**
 * 订阅后端状态推送里的若干主题（UX#5）。
 *
 * 三个必须处理的点，每个都对应一类真实故障：
 * 1. **浏览器预览模式直接 return** —— 那里没有 Tauri 事件系统。
 * 2. **React 18 StrictMode 下 effect 会双挂载**，而 `listen` 返回的是 `Promise<UnlistenFn>`。
 *    若不用 `dead` 标志接住「resolve 时组件已卸载」这种情况，每次热更新都会泄漏一个监听器，
 *    表现为同一个事件被处理 N 次（N 随开发时长增长），而生产构建下又复现不了。
 * 3. **回调用 ref 存** —— 调用方几乎都传内联箭头函数，直接进依赖数组会每次渲染重订阅。
 */
export function useBackendEvent(topics: BackendTopic[], fn: () => void) {
  const fnRef = useRef(fn);
  fnRef.current = fn;
  // topics 用 join 进依赖数组：调用方传的是内联数组字面量，直接放进去等于每次渲染都重订阅。
  const topicKey = topics.join(",");

  useEffect(() => {
    if (!isTauri()) return;
    let dead = false;
    let unlisten: (() => void) | null = null;
    const wanted = topicKey.split(",");

    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<StateChangedPayload>(EVENT_NAME, (e) => {
          if (wanted.includes(e.payload.topic)) fnRef.current();
        }),
      )
      .then((un) => {
        if (dead) un();
        else unlisten = un;
      })
      .catch((err) => {
        // 订阅失败不是致命的：兜底轮询仍在。但要留痕，否则「实时性没了」无从查起。
        console.error("listen state-changed failed", err);
      });

    return () => {
      dead = true;
      unlisten?.();
    };
  }, [topicKey]);
}

/**
 * App 层挂一次的全局订阅（UX#5）。
 *
 * ## 刻意不含 loadSettings 与大脑配置
 *
 * `config` 是从后端 `mutate_and_persist` 这个 choke point 发出来的，用户在设置页 / 大脑页
 * 改**任何**一个字段都会触发它。而 SettingsPage 与 BrainPage 都持有自己的局部草稿
 * （useState + 乐观更新），若在这里把磁盘值灌回 store 并往下传，就会出现
 * **「用户正在输入的值，被自己刚才那次保存触发的回声顶掉」** —— 一种极难复现、
 * 用户只会描述成「打字时偶尔跳回去」的问题。
 *
 * 真正需要跟随 settings 的只有「**后端自己**改了 settings」这一类（托盘快切模型/强度），
 * 所以后端为它单开了窄口径的 `settings` 主题。
 */
export function useBackendEvents() {
  // 用 getState() 取 action 而不是 selector 订阅：action 是稳定引用，
  // 但放进依赖数组会让这个 effect 的意图变得含糊。
  useBackendEvent(["config", "proxy", "health"], () => {
    void useStore.getState().refreshCategory();
  });
  useBackendEvent(["settings"], () => {
    void useStore.getState().loadSettings();
  });

  // `vault` 与 `logs` 刻意**不在 hub 里**处理：
  // - vault（密钥库锁态）目前是 CategoryPage 的局部 state，由该页自己订阅更贴近数据所在处；
  // - logs 若在 hub 处理，会变成「日志页没打开也每次都重拉 500 条事件」，
  //   正好抵消掉本项省下来的 IPC。交给 LogsPage / CategoryPage 各自订阅。

  // 注册完成后立刻手动跑一次刷新。
  //
  // 从 App 挂载到 `listen` 的 Promise resolve 之间有几十毫秒窗口，而 **Tauri 事件不缓冲** ——
  // 这期间后端发出的 emit 会永久丢失。不补这一次的话，恰好在启动瞬间发生的状态变化
  // （例如启动时的自启动对账、密钥库锁态对账）要等 30s 兜底才会显示出来。
  useEffect(() => {
    void useStore.getState().refreshCategory();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 兜底：推送丢了/漏了也不会一直错下去。
  usePolling(() => {
    void useStore.getState().refreshCategory();
  }, FALLBACK_POLL_MS);
}
