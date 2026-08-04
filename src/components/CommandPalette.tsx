import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  Search,
  Terminal,
  MonitorSmartphone,
  Code2,
  Brain,
  ScrollText,
  Building2,
  Settings,
  UserRound,
  Play,
  Square,
  Star,
  FolderOpen,
  KeyRound,
  CornerDownLeft,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { CategoryType, ProviderKey, ProxyState } from "@/types";
import type { NavKey } from "@/components/Sidebar";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { fuzzyMatchAny } from "@/lib/fuzzyMatch";
import { openLogDir } from "@/lib/openLogDir";

const CATEGORIES: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];

const NAV_ICONS: Record<NavKey, LucideIcon> = {
  "claude-cli": Terminal,
  "claude-desktop": MonitorSmartphone,
  codex: Code2,
  brain: Brain,
  logs: ScrollText,
  vendors: Building2,
  settings: Settings,
  about: UserRound,
};

/** 一条候选项。`run` 返回 false 表示执行失败（调用方据此不弹成功提示）。 */
interface Item {
  id: string;
  icon: LucideIcon;
  title: string;
  /** 副标题，用于说明「匹配到的是哪个字段」或补充上下文 */
  subtitle?: string;
  /** 分组标题（渲染时按此聚合） */
  group: string;
  /** 参与模糊匹配的字段。label 会显示在 subtitle 里 */
  haystack: { label: string; value: string }[];
  run: () => void | Promise<void>;
}

interface Props {
  onClose: () => void;
  onNavigate: (key: NavKey) => void;
  onEditKey: (k: ProviderKey) => void;
  onAddKey: () => void;
}

/**
 * 全局命令面板（UX#7，Ctrl/Cmd+K）。
 *
 * **零新增依赖**：本项目是桌面应用、包体积敏感，且 UI 组件全是自建的。
 * 也刻意**不复用 Combobox** —— 那个组件的 typing/shownValue 双态语义是为「可编辑值字段」
 * 设计的（值即文本），而命令面板的查询串与选中项之间没有值绑定关系。硬合并会同时弄脏
 * 映射下拉与兜底模型下拉这两个已经稳定的场景。
 *
 * ## 跨分类操作的锁：必须 await loadCategory
 *
 * store 里的 `setPrimaryKey` / `startProxy` / `stopProxy` 全部硬绑 `get().activeCategory`，
 * 而 `keys` 只装当前分类那一份。跨分类操作若只 `setActiveCategory` 就立刻调它们：
 * - `setPrimaryKey` 在旧的 keys 里 findIndex 找不到目标 → `idx <= 0` **静默返回**，
 *   用户看到面板关了、主 Key 没变、连个报错都没有；
 * - `startProxy` 那次在途的 loadCategory 会在启动之后把 proxy 用**启动前**的旧值覆盖回来，
 *   表现为「点了启动、状态条闪一下又变回未运行」，而代理其实已经起来了 ——
 *   这是最容易被误判成后端 bug 的一类现象。
 *
 * 所以本文件里每个跨分类动作都严格走 `await loadCategory(cat)` 再执行。
 */
export function CommandPalette({ onClose, onNavigate, onEditKey, onAddKey }: Props) {
  const t = useT();
  const showToast = useStore((s) => s.showToast);
  const loadCategory = useStore((s) => s.loadCategory);
  const setPrimaryKey = useStore((s) => s.setPrimaryKey);
  const startProxy = useStore((s) => s.startProxy);
  const stopProxy = useStore((s) => s.stopProxy);

  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const [allKeys, setAllKeys] = useState<{ cat: CategoryType; keys: ProviderKey[] }[]>([]);
  const [proxies, setProxies] = useState<Partial<Record<CategoryType, ProxyState>>>({});
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  /**
   * 上一次**真实**的鼠标位置，用于识别「鼠标其实没动」的伪 mousemove。
   *
   * 不加这道判断会有一个很难描述的抖动 bug：键盘 ↑↓ 会触发 `scrollIntoView` 滚动列表，
   * 滚动让另一行移动到静止的鼠标指针**下方**，浏览器据此派发一个 mousemove ——
   * 于是 `onMouseMove` 把 cursor 改成鼠标下那一行，**键盘选择被鼠标抢走**。
   * 表现为连按方向键时选中项乱跳、按住不放时来回抖，而用户的鼠标根本没碰。
   *
   * 判据是坐标：伪 mousemove 的 clientX/clientY 与上次完全相同，直接忽略。
   */
  const lastMouse = useRef<{ x: number; y: number } | null>(null);

  // 面板是条件渲染的（`{paletteOpen && <CommandPalette/>}`），所以这个 effect
  // 天然等价于「每次打开刷新一次」，不需要额外的 open 依赖。
  useEffect(() => {
    let alive = true;
    void (async () => {
      // 必须 allSettled 而不是 all：一个分类读失败就整块空白，
      // 与 store.loadCategory 的既有容错口径也不一致（那边是单分类失败不影响其余）。
      const keyResults = await Promise.allSettled(CATEGORIES.map((c) => api.listKeys(c)));
      const proxyResults = await Promise.allSettled(CATEGORIES.map((c) => api.getProxyState(c)));
      if (!alive) return;
      setAllKeys(
        CATEGORIES.map((cat, i) => ({
          cat,
          keys: keyResults[i].status === "fulfilled" ? keyResults[i].value : [],
        })),
      );
      const px: Partial<Record<CategoryType, ProxyState>> = {};
      CATEGORIES.forEach((cat, i) => {
        const r = proxyResults[i];
        if (r.status === "fulfilled") px[cat] = r.value;
      });
      setProxies(px);
    })();
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => inputRef.current?.focus(), []);

  /** 执行一个动作：先关面板（让界面立刻有反应），再跑。 */
  const exec = (fn: () => void | Promise<void>) => {
    onClose();
    void Promise.resolve(fn()).catch((e) => {
      showToast("error", String((e as Error)?.message ?? e));
    });
  };

  const items = useMemo<Item[]>(() => {
    const out: Item[] = [];

    // ---- 跳页 ----
    for (const key of Object.keys(NAV_ICONS) as NavKey[]) {
      out.push({
        id: `nav:${key}`,
        icon: NAV_ICONS[key],
        title: t(`nav.${key}`),
        group: t("palette.groupNav"),
        haystack: [{ label: "", value: t(`nav.${key}`) }],
        run: () => onNavigate(key),
      });
    }

    // ---- Key：跨三分类，可搜备注名 / 厂商 / 模型名 ----
    for (const { cat, keys } of allKeys) {
      for (const k of keys) {
        const models = k.models.map((m) => m.realName);
        const outward = k.mappings.map((m) => m.expectedName).filter(Boolean);
        out.push({
          id: `key:${k.id}`,
          icon: KeyRound,
          title: k.name,
          group: t("palette.groupKeys"),
          haystack: [
            { label: "", value: k.name },
            { label: t("palette.fieldVendor"), value: k.vendor },
            ...models.map((m) => ({ label: t("palette.fieldModel"), value: m })),
            ...outward.map((m) => ({ label: t("palette.fieldOutward"), value: m })),
          ],
          run: async () => {
            // 严格「先切分类、再开编辑器」：反过来的话 KeyEditor 挂载时拿到的是旧分类。
            onNavigate(cat as NavKey);
            await loadCategory(cat);
            onEditKey(k);
          },
        });

        // 切主 Key：只对**启用**的 Key 提供（禁用的不进候选池，设为主毫无意义）
        if (k.enabled) {
          out.push({
            id: `primary:${k.id}`,
            icon: Star,
            title: t("palette.setPrimary", { name: k.name }),
            subtitle: t(`nav.${cat}`),
            group: t("palette.groupActions"),
            haystack: [
              { label: "", value: `${t("palette.setPrimary", { name: k.name })}` },
              { label: "", value: k.name },
            ],
            run: async () => {
              onNavigate(cat as NavKey);
              await loadCategory(cat); // 必须等：否则 setPrimaryKey 在旧 keys 里找不到目标而静默返回
              const ok = await setPrimaryKey(k.id);
              if (ok) showToast("success", t("palette.primaryDone", { name: k.name }));
            },
          });
        }
      }
    }

    // ---- 启停代理：按分类 ----
    for (const cat of CATEGORIES) {
      const running = proxies[cat]?.status === "running";
      out.push({
        id: `proxy:${cat}`,
        icon: running ? Square : Play,
        title: running
          ? t("palette.stopProxy", { cat: t(`nav.${cat}`) })
          : t("palette.startProxy", { cat: t(`nav.${cat}`) }),
        subtitle: running ? t("proxy.running") : t("proxy.stopped"),
        group: t("palette.groupActions"),
        haystack: [
          {
            label: "",
            value: running
              ? t("palette.stopProxy", { cat: t(`nav.${cat}`) })
              : t("palette.startProxy", { cat: t(`nav.${cat}`) }),
          },
        ],
        run: async () => {
          onNavigate(cat as NavKey);
          await loadCategory(cat); // 必须等：否则在途的 loadCategory 会把 proxy 覆盖回启动前的旧值
          if (running) await stopProxy();
          else await startProxy();
        },
      });
    }

    // ---- 新建 Key / 打开日志目录 ----
    out.push({
      id: "action:addKey",
      icon: KeyRound,
      title: t("palette.addKey"),
      group: t("palette.groupActions"),
      haystack: [{ label: "", value: t("palette.addKey") }],
      run: () => onAddKey(),
    });
    out.push({
      id: "action:logDir",
      icon: FolderOpen,
      title: t("settings.logOpenDir"),
      group: t("palette.groupActions"),
      haystack: [{ label: "", value: t("settings.logOpenDir") }],
      run: () => openLogDir(),
    });

    return out;
  }, [
    allKeys,
    proxies,
    t,
    onNavigate,
    onEditKey,
    onAddKey,
    loadCategory,
    setPrimaryKey,
    startProxy,
    stopProxy,
    showToast,
  ]);

  /** 过滤 + 排序。空查询时保持声明顺序（跳页在前，便于纯键盘导航）。 */
  const filtered = useMemo(() => {
    const scored = items
      .map((it) => {
        const m = fuzzyMatchAny(query, it.haystack);
        return m ? { it, score: m.score, hitField: m.field, hitValue: m.value } : null;
      })
      .filter((x): x is NonNullable<typeof x> => x !== null);
    if (query.trim()) scored.sort((a, b) => b.score - a.score);
    return scored.slice(0, 50);
  }, [items, query]);

  // 查询变化后光标要回到第一条，否则会停在一个已被过滤掉的下标上（表现为「按回车没反应」）。
  useEffect(() => setCursor(0), [query]);

  // 让选中项始终可见。用 block:"nearest" 而不是 scrollIntoView() 默认值，
  // 后者会把整个面板在页面里滚动一下。
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${cursor}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((c) => Math.min(c + 1, filtered.length - 1));
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => Math.max(c - 1, 0));
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const target = filtered[cursor];
      if (target) exec(target.it.run);
      return;
    }
    // 焦点陷阱：面板里只有输入框一个可聚焦元素，Tab 应该什么都不做。
    // 列表行用 div + role="option"（不是 button），否则 Tab 会在几十条候选里逐个走。
    if (e.key === "Tab") e.preventDefault();
  };

  // 分组渲染：记录每个分组第一次出现的位置，在那里插一个组标题。
  const groupFirstIdx = new Map<string, number>();
  filtered.forEach((f, i) => {
    if (!groupFirstIdx.has(f.it.group)) groupFirstIdx.set(f.it.group, i);
  });

  return createPortal(
    <div
      // z-[90]：必须夹在弹窗的 z-50 与 Toast 的 z-[100] 之间 ——
      // 低了会被抽屉盖住，高了会挡住执行失败的红色提示。
      className="fixed inset-0 z-[90] flex items-start justify-center bg-black/30 pt-[12vh]"
      // 遮罩关闭必须用 onMouseDown + target===currentTarget，禁止 onClick={onClose}：
      // 否则在输入框里拖选文字、松手落在遮罩上会误关面板并丢掉已输入的查询。
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="flex max-h-[70vh] w-[min(640px,92vw)] flex-col overflow-hidden rounded-lg border border-border bg-surface shadow-2xl"
        onKeyDown={onKeyDown}
        role="dialog"
        aria-modal="true"
        aria-label={t("palette.title")}
      >
        <div className="flex items-center gap-2 border-b border-border px-3.5 py-3">
          <Search size={16} className="shrink-0 text-text-muted" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("palette.placeholder")}
            className="flex-1 bg-transparent text-sm text-text-primary outline-none placeholder:text-text-muted"
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-activedescendant={filtered[cursor] ? `palette-opt-${cursor}` : undefined}
          />
          <kbd className="shrink-0 rounded border border-border bg-background px-1.5 py-0.5 text-[10px] text-text-muted">
            Esc
          </kbd>
        </div>

        <div ref={listRef} id="palette-list" role="listbox" className="min-h-0 flex-1 overflow-y-auto py-1">
          {filtered.length === 0 && (
            <div className="px-4 py-8 text-center text-xs text-text-muted">{t("palette.noResults")}</div>
          )}
          {filtered.map((f, i) => {
            const Icon = f.it.icon;
            const selected = i === cursor;
            const showGroup = groupFirstIdx.get(f.it.group) === i;
            // 副标题优先显示「匹配到的是哪个字段」——用户搜模型名时最想确认的就是
            // 「这条 Key 是因为哪个模型被搜出来的」。
            const sub = f.hitField ? `${f.hitField}: ${f.hitValue}` : f.it.subtitle;
            return (
              <div key={f.it.id}>
                {showGroup && (
                  <div className="px-3.5 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wide text-text-muted">
                    {f.it.group}
                  </div>
                )}
                <div
                  id={`palette-opt-${i}`}
                  data-idx={i}
                  role="option"
                  aria-selected={selected}
                  onMouseMove={(e) => {
                    // 只有鼠标真的移动了才接管选中项，详见 lastMouse 注释
                    const p = lastMouse.current;
                    if (p && p.x === e.clientX && p.y === e.clientY) return;
                    lastMouse.current = { x: e.clientX, y: e.clientY };
                    setCursor(i);
                  }}
                  onClick={() => exec(f.it.run)}
                  className={`mx-1.5 flex cursor-pointer items-center gap-2.5 rounded-control px-2.5 py-2 ${
                    selected ? "bg-primary/12" : ""
                  }`}
                >
                  <Icon size={15} className="shrink-0 text-text-secondary" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm text-text-primary">{f.it.title}</div>
                    {sub && <div className="truncate text-[11px] text-text-muted">{sub}</div>}
                  </div>
                  {selected && <CornerDownLeft size={13} className="shrink-0 text-text-muted" />}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>,
    document.body,
  );
}
