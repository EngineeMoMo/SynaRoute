import { usePolling } from "@/lib/usePolling";
import { useBackendEvent, FALLBACK_POLL_MS } from "@/lib/useBackendEvents";
import { useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "@/store";
import { KeyCard } from "@/components/KeyCard";
import { ProxyStatusBar } from "@/components/ProxyStatusBar";
import { CcSwitchImportDialog } from "@/components/CcSwitchImportDialog";
import { Button } from "@/components/ui/Button";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
// 与状态条共用：两份实现必然漂移，而漂移后果是「状态条列出的模型在某备用 Key 上其实路由不了」
import { keyExpectedSet } from "@/lib/modelSets";
import type { EventLogEntry, ProviderKey } from "@/types";
import { Plus, AlertTriangle, Inbox, X, Database } from "lucide-react";

/** 分类主页：代理状态条 + 模型映射兜底提示 + Key 卡片列表 */
export function CategoryPage({ onAddKey, onEditKey, onOpenLogs }: {
  onAddKey: () => void;
  onEditKey: (k: ProviderKey) => void;
  /** 跳转到运行日志页（「最近失败原因」横幅的「查看详情」用）。 */
  onOpenLogs: () => void;
}) {
  // 细粒度订阅（勿改回整店解构 `useStore()`）：整店订阅会让本页在 LogsPage 每 2s
  // 刷新 events 时也全量重渲染——连同它下面的全部 KeyCard。
  const activeCategory = useStore((s) => s.activeCategory);
  const keys = useStore((s) => s.keys);
  const proxy = useStore((s) => s.proxy);
  const loading = useStore((s) => s.loading);
  const refreshCategory = useStore((s) => s.refreshCategory);
  const t = useT();
  const [gapDialogOpen, setGapDialogOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);

  // 兜底轮询（UX#5）。真正的实时性由 App 层的 useBackendEvents 承担：代理启停、
  // 配置落盘、健康态翻转会被后端主动推过来、界面即时跟随。这里退成 30s 只为兜住
  // 「推送队列满被丢」与「监听注册前的窗口」两种漏网情况。
  // 窗口不可见时自动停表（见 usePolling）。挂载时 usePolling 自己会立即执行一次。
  usePolling(() => void refreshCategory(), FALLBACK_POLL_MS);

  // 排序：按优先级
  const sorted = useMemo(
    () => [...keys].sort((a, b) => a.priority - b.priority),
    [keys]
  );

  // 模型映射兜底检查（FR-006a）：统计启用 Key 中，各期望模型的覆盖缺口
  const gaps = useMemo(() => detectMappingGaps(keys.filter((k) => k.enabled)), [keys]);

  // 当前处于熔断中的 Key（FR-028）：breakerUntil 存在且未过期。常驻警告条的数据源。
  // 熔断是「连续失败自动暂停」的状态，必须常驻可见——否则用户看到某 Key 没被路由，
  // 却不知道它正被暂停、还以为是配置错。
  const trippedKeys = useMemo(
    () =>
      keys.filter((k) => {
        if (!k.health.breakerUntil) return false;
        return Date.now() < k.health.breakerUntil;
      }),
    [keys]
  );

  /**
   * 分类切换时作废「所有在途异步结果」的代际号。
   *
   * 为什么必须有：本页在三个分类间切换时**不会卸载**（App.tsx 对三个分类渲染的是同一个
   * `<CategoryPage>`、没给 `key`，切换只改 store 里的 activeCategory），所以：
   *
   * 1. 切换前发出的 IPC 请求会在切换**之后**才 resolve，然后把上一个分类的数据
   *    `setState` 进来——于是「Codex 刚刚转发失败」那条红色横幅会挂在 Claude CLI 页上，
   *    「桌面端被 cc-switch 接管」的警告会出现在根本没有这回事的 Codex 页上；
   * 2. 各条状态本身也不会因为换了分类而自动清空。
   *
   * 尤其阴险的是 takeover 那条：它的轮询 `enabled = isDesktop`，切到非桌面端后定时器就停了，
   * **没有下一次轮询来纠正它**，那条假警告会一直挂到用户再切回桌面端为止。
   *
   * 故：切换分类时 ① 递增代际号，在途结果回来时代际对不上就丢弃；② 立即清空这些
   * 跨分类不通用的状态，不等下一次轮询（等的话最坏要顶着错误信息 5 秒）。
   */
  const genRef = useRef(0);
  const [takeover, setTakeover] = useState<string | null>(null);
  const [vaultLocked, setVaultLocked] = useState(false);
  const [recentFailure, setRecentFailure] = useState<EventLogEntry | null>(null);
  useEffect(() => {
    genRef.current += 1;
    setTakeover(null);
    setRecentFailure(null);
    // vaultLocked 刻意**不清**：密钥库锁没锁与分类无关，是全局状态，清了反而会闪一下。
  }, [activeCategory]);

  // 桌面端接入是否已被其他工具接管（cc-switch 重写 _meta.json）。
  //
  // **这条只能轮询**（UX#5 的另一个例外）：判据是**外部文件**被别的程序改写，
  // 后端不知道它何时变，除非加一套文件监视 —— 那是另一套机制，而接管是低频事件。
  //
  // 拉长到 30s 不会让用户等：`usePolling` 有「窗口重新可见时立即补一次」的语义，
  // 而这里的真实场景恰恰是「用户切到 cc-switch 点了一下，再切回 SynaRoute」——
  // 切回来那一刻就会刷新，根本走不到周期。
  const isDesktop = activeCategory === "claude-desktop";
  // `enabled` 只对桌面端开：其余分类没有「被 cc-switch 接管」这回事，不必白问。
  usePolling(
    () => {
      const gen = genRef.current;
      void api
        .getToolConfigPreview("claude-desktop")
        .then((p) => {
          if (gen === genRef.current) setTakeover(p.takeoverWarning ?? null);
        })
        .catch(() => {
          // 预览读不出来（路径不存在等）不影响主界面，静默即可
        });
    },
    FALLBACK_POLL_MS,
    isDesktop,
  );

  // 密钥库是否锁着（主口令模式尚未解锁）。锁着时**每一次转发都会失败**，
  // 而失败原因藏在运行日志里 —— 必须在最显眼的位置常驻提示，否则用户会以为 Key 配错了。
  // 这条与分类无关（全局密钥库），故不参与代际作废。
  //
  // 锁/解锁由后端 `vault` 主题即时推送（在设置页点「立即锁定」后切回本页应已是最新），
  // 30s 轮询只作兜底。刻意留在本页而不搬进 App 层 hub：状态就在这里用，
  // 提到全局反而多一层传递。
  const refreshVault = () => {
    void api
      .getMasterPasswordState()
      .then((s) => setVaultLocked(s.enabled && s.locked))
      .catch(() => {
        // 读不到就不提示（宁可漏提示，也不要因一次 IPC 抖动弹个假警告）
      });
  };
  useBackendEvent(["vault"], refreshVault);
  usePolling(refreshVault, FALLBACK_POLL_MS);

  /**
   * 最近一次转发失败（UX#11）。
   *
   * 为什么要常驻在这里：转发失败时用户在客户端只看到 502/529 之类的状态码，
   * 而真实原因（哪个 Key、什么错、是鉴权还是限流）藏在运行日志页里——用户得先想到去翻日志。
   * 这条把它提到最显眼的位置，与上面 vaultLocked / takeover 两条警告同一模式
   * （那个模式已被证明好用）。
   *
   * **为什么走后端 `recentFailure` 而不在前端遍历 `events`**：`refreshCategory`（本页 5s 轮询）
   * 只拉 keys + proxy，**不含 events**——events 只在 `loadCategory`（挂载/切分类）时拉一次。
   * 若在前端算，这条横幅就会一直显示挂载那一刻的旧快照、不随新失败更新；
   * 而把 `listAllEvents` 加进 5s 轮询又会把 P1-6 刚消掉的「每 2s 搬 500 条」重新引回来。
   * 后端这个命令只返回**一条**已剥离 trace 的事件，代价可忽略。
   *
   * 结果要过 `genRef` 代际校验：切分类时本页不卸载，上一个分类的在途结果若直接写进来，
   * 就会把「Codex 刚失败」的红条挂到 Claude CLI 页上（详见 genRef 的注释）。
   */
  const FRESH_MS = 5 * 60 * 1000; // 5 分钟内的失败才算「最近」
  const refreshRecentFailure = () => {
    const gen = genRef.current;
    const cat = activeCategory;
    void api
      .recentFailure(cat, FRESH_MS)
      .then((ev) => {
        if (gen === genRef.current) setRecentFailure(ev);
      })
      .catch(() => {
        // 读不到就不提示（宁可漏提示，也不要因一次 IPC 抖动弹个假警告）
      });
  };
  // 订阅 `logs`：转发失败会写事件，所以这条横幅要跟着日志走而不是跟着配置走。
  // 后端已对 logs 主题限速 1500ms（见 events.rs 的 min_interval_ms），
  // 密集失败时不会把这个查询打成高频。
  useBackendEvent(["logs"], refreshRecentFailure);
  usePolling(refreshRecentFailure, FALLBACK_POLL_MS);

  return (
    <div className="flex h-full flex-col">
      <ProxyStatusBar proxy={proxy} />

      <div className="flex items-center justify-between px-6 pb-3 pt-4">
        <div>
          <h1 className="text-lg font-semibold text-text-primary">{t(`nav.${activeCategory}`)}</h1>
          <p className="text-xs text-text-muted">
            {t("category.keyCount", { total: keys.length, enabled: keys.filter((k) => k.enabled).length })}
          </p>
        </div>
        <Button size="icon" onClick={onAddKey} title={t("category.addKey")} aria-label={t("category.addKey")}>
          <Plus size={18} />
        </Button>
      </div>

      {/* 从 cc-switch 导入历史 Key：只读对方库、导入后不接入（详见 CcSwitchImportDialog） */}
      {importOpen && (
        <CcSwitchImportDialog
          onClose={() => setImportOpen(false)}
          onImported={() => void refreshCategory()}
        />
      )}


      {/* 桌面端接入被其他工具接管（cc-switch 一被点开就整份重写 _meta.json）：
          档还在磁盘上、代理也在跑，但桌面端实际走的是别人那一档 → 表现为「接入了但不生效」。
          这是该无头案的唯一线索，故常驻在列表顶部而非只藏在预览弹窗里。 */}
      {/* 密钥库锁定：比其他警告更靠前，因为它让**全部**转发失败，不是某一项配置不完美。
          用 danger 而非 warning，且给出可执行动作。 */}
      {vaultLocked && (
        <div className="mx-6 mb-2 flex items-start gap-2 rounded-control border border-danger/30 bg-danger/8 px-3 py-2 text-xs text-danger">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <span className="flex-1 leading-relaxed">{t("master.lockedBanner")}</span>
        </div>
      )}

      {takeover && (
        <div className="mx-6 mb-2 flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-xs text-warning">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <span className="flex-1 leading-relaxed">{takeover}</span>
        </div>
      )}

      {/* 最近一次转发失败（UX#11）：客户端只会显示 502/529 之类的状态码，真实原因
          （哪个 Key、鉴权还是限流）此前只藏在运行日志页里，用户得先想到去翻。
          放在 vaultLocked / takeover 之后：那两条是「全盘不可用」，这条是「刚刚失败过一次」。 */}
      {recentFailure && (
        <div className="mx-6 mb-2 flex items-start gap-2 rounded-control border border-danger/30 bg-danger/8 px-3 py-2 text-xs text-danger">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <div className="flex-1 leading-relaxed">
            <span className="font-medium">{t("category.recentFailure")}</span>
            <span className="ml-1 break-all">{recentFailure.detail}</span>
          </div>
          <button
            type="button"
            onClick={onOpenLogs}
            className="shrink-0 whitespace-nowrap underline underline-offset-2 hover:opacity-80"
          >
            {t("category.recentFailureView")}
          </button>
        </div>
      )}

      {/* 熔断中的 Key（FR-028 常驻告警）：连续失败已自动暂停使用，其他 Key 接管。
          常驻而非一次性提示——熔断窗口最长 60s，用户切回来时若已恢复不该看到假警告，
          但若还在熔断中，必须一眼看见「这条为什么没被用」。 */}
      {trippedKeys.length > 0 && (
        <div className="mx-6 mb-2 flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-xs text-warning">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <div className="flex-1 leading-relaxed">
            <span className="font-medium">{t("category.trippedKeys")}</span>
            <span className="ml-1 break-all">
              {trippedKeys.map((k) => k.name).join("、")}
            </span>
          </div>
        </div>
      )}

      {/* 映射缺口提示（FR-006a）：只保留一行精简条，明细收进弹窗，避免条目过多撑爆页面 */}
      {gaps.length > 0 && (
        <button
          type="button"
          onClick={() => setGapDialogOpen(true)}
          className="mx-6 mb-2 flex items-center gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-left text-xs text-warning hover:bg-warning/12"
        >
          <AlertTriangle size={14} className="shrink-0" />
          <span className="flex-1 font-medium">{t("category.mappingGapSummary", { count: gaps.length })}</span>
          <span className="shrink-0 underline underline-offset-2">{t("category.mappingGapView")}</span>
        </button>
      )}

      {gapDialogOpen && (
        <MappingGapDialog gaps={gaps} onClose={() => setGapDialogOpen(false)} />
      )}

      {/* pb-24 而非 pb-6：右下角有常驻的快捷面板悬浮按钮（48px + 24px 边距），
          底部留白不足时它会压住最后一张卡片的「启用」开关，用户滚到底也点不到
          （实测过：FAB 矩形与最后一个 Switch 重叠）。 */}
      <div className="flex-1 space-y-2 overflow-y-auto px-6 pb-24">
        {loading && <div className="py-10 text-center text-sm text-text-muted">{t("common.loading")}</div>}

        {!loading && sorted.length === 0 && (
          <div className="flex flex-col items-center justify-center gap-3 py-16 text-text-muted">
            <Inbox size={40} />
            <p className="text-sm">{t("category.empty")}</p>
            <div className="flex gap-2">
              <Button variant="secondary" onClick={onAddKey}>
                <Plus size={16} /> {t("category.addFirst")}
              </Button>
              {/* 空列表是最需要「从 cc-switch 导入」的场景：老用户往往已在 cc-switch 里配好了 */}
              <Button variant="outline" onClick={() => setImportOpen(true)}>
                <Database size={16} /> 从 cc-switch 导入
              </Button>
            </div>
          </div>
        )}

        {!loading && sorted.length > 0 && (
          <div className="flex justify-end pb-1">
            <Button variant="ghost" size="sm" onClick={() => setImportOpen(true)}>
              <Database size={14} /> 从 cc-switch 导入
            </Button>
          </div>
        )}

        {!loading &&
          sorted.map((k, i) => (
            <KeyCard
              key={k.id}
              k={k}
              onEdit={onEditKey}
              isFirst={i === 0}
              isLast={i === sorted.length - 1}
            />
          ))}
      </div>
    </div>
  );
}

interface Gap {
  expected: string;
  missingKeys: string[];
}

/**
 * 模型可选性检查（FR-006a）——与后端 /v1/models 发现端点的「交集」口径对齐。
 *
 * Claude CLI 的 `/model` 选择器只展示各启用 Key「可服务模型」的**交集**（共有的那批），
 * 这样选中任意模型都能在所有候选 Key 上路由，无感切换不会“模型不存在”。
 *
 * 这里以主 Key（优先级最高的启用 Key，也是交集的排序/回退基准）的模型为准，标出哪些
 * 因为在某些备用 Key 上缺映射而**没进交集、不会出现在选择器里**。用户去对应 Key 补一条
 * 映射，该模型即可进入选择器。一个主 Key 模型只要不被任何备用 Key 缺失，就已在交集中、无需提示。
 */
function detectMappingGaps(enabledKeys: ProviderKey[]): Gap[] {
  if (enabledKeys.length < 2) return [];

  // 主 Key = 优先级最高（数值最小）的启用 Key（后端空交集时也回退到它）
  const primary = [...enabledKeys].sort((a, b) => a.priority - b.priority)[0];
  const primaryModels = keyExpectedSet(primary);

  const backups = enabledKeys
    .filter((k) => k.id !== primary.id)
    .map((k) => ({ key: k, set: keyExpectedSet(k) }));

  // 主 Key 的每个模型：凡是被某个备用 Key 缺失的，就没进交集 → 不会出现在选择器里
  const gaps: Gap[] = [];
  for (const expected of primaryModels) {
    const missing = backups
      .filter(({ set }) => !set.has(expected))
      .map(({ key }) => key.name);
    if (missing.length > 0) {
      gaps.push({ expected, missingKeys: missing });
    }
  }
  return gaps;
}

/** 映射缺口明细弹窗：条目多时页面顶部只留精简条，明细在此滚动查看，避免撑爆页面 */
function MappingGapDialog({ gaps, onClose }: { gaps: Gap[]; onClose: () => void }) {
  const t = useT();
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="mx-4 flex max-h-[70vh] w-full max-w-lg flex-col rounded-card border border-border bg-surface shadow-xl">
        <div className="flex items-start justify-between gap-2 border-b border-border px-5 py-4">
          <div className="flex items-start gap-2">
            <AlertTriangle size={18} className="mt-0.5 shrink-0 text-warning" />
            <div>
              <h2 className="text-base font-semibold text-text-primary">{t("category.mappingGapTitle")}</h2>
              <p className="mt-0.5 text-xs text-text-muted">{t("category.mappingGapHint")}</p>
            </div>
          </div>
          <button onClick={onClose} className="shrink-0 rounded p-1 text-text-muted hover:bg-surface-hover">
            <X size={18} />
          </button>
        </div>
        <div className="flex-1 space-y-1.5 overflow-y-auto px-5 py-4">
          {gaps.map((g) => (
            <div key={g.expected} className="rounded-control bg-warning/8 px-3 py-2 text-xs text-text-secondary">
              {t("category.mappingGapItem", { expected: g.expected, keys: g.missingKeys.join("、") })}
            </div>
          ))}
        </div>
        <div className="flex justify-end border-t border-border px-5 py-3">
          <Button variant="ghost" onClick={onClose}>{t("common.close")}</Button>
        </div>
      </div>
    </div>
  );
}
