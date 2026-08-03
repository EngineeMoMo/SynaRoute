import { usePolling } from "@/lib/usePolling";
import { useEffect, useMemo, useState } from "react";
import { useStore } from "@/store";
import { KeyCard } from "@/components/KeyCard";
import { ProxyStatusBar } from "@/components/ProxyStatusBar";
import { CcSwitchImportDialog } from "@/components/CcSwitchImportDialog";
import { Button } from "@/components/ui/Button";
import { Combobox } from "@/components/ui/Combobox";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { ProviderKey } from "@/types";
import { Plus, AlertTriangle, Inbox, X, Database } from "lucide-react";

/** 分类主页：代理状态条 + 模型映射兜底提示 + Key 卡片列表 */
export function CategoryPage({ onAddKey, onEditKey }: {
  onAddKey: () => void;
  onEditKey: (k: ProviderKey) => void;
}) {
  // 细粒度订阅（勿改回整店解构 `useStore()`）：整店订阅会让本页在 LogsPage 每 2s
  // 刷新 events 时也全量重渲染——连同它下面的全部 KeyCard。
  const activeCategory = useStore((s) => s.activeCategory);
  const keys = useStore((s) => s.keys);
  const proxy = useStore((s) => s.proxy);
  const loading = useStore((s) => s.loading);
  const refreshCategory = useStore((s) => s.refreshCategory);
  const settings = useStore((s) => s.settings);
  const setActiveModel = useStore((s) => s.setActiveModel);
  const setActiveEffort = useStore((s) => s.setActiveEffort);
  const t = useT();
  const [gapDialogOpen, setGapDialogOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);

  // 窗口不可见时自动停表（见 usePolling）。注意这里**不再**在挂载时单独 refresh 一次——
  // usePolling 自己会立即执行一次。
  usePolling(() => void refreshCategory(), 5000);

  // 排序：按优先级
  const sorted = useMemo(
    () => [...keys].sort((a, b) => a.priority - b.priority),
    [keys]
  );

  // 模型映射兜底检查（FR-006a）：统计启用 Key 中，各期望模型的覆盖缺口
  const gaps = useMemo(() => detectMappingGaps(keys.filter((k) => k.enabled)), [keys]);

  // 应用内「当前模型」下拉：仅 Codex 需要（其模型菜单是内置固定清单、拉不到中转模型）。
  // 候选与后端 /v1/models「交集」口径一致（discoverableModels），故选中的名字在所有候选 Key 都能路由。
  const discoverable = useMemo(
    () => discoverableModels(keys.filter((k) => k.enabled)),
    [keys]
  );
  const activeModel = settings?.activeModels?.[activeCategory] ?? "";
  const activeEffort = settings?.activeEfforts?.[activeCategory] ?? "";

  // 桌面端接入是否已被其他工具接管（cc-switch 重写 _meta.json）。
  // 只对桌面端查，且跟随 5s 轮询刷新——用户可能在 SynaRoute 开着的时候去点 cc-switch。
  const [takeover, setTakeover] = useState<string | null>(null);
  const isDesktop = activeCategory === "claude-desktop";
  useEffect(() => {
    if (!isDesktop) setTakeover(null);
  }, [isDesktop]);
  // `enabled` 只对桌面端开：其余分类没有「被 cc-switch 接管」这回事，不必白问。
  // 窗口不可见时停表（见 usePolling）。
  usePolling(
    () => {
      void api
        .getToolConfigPreview("claude-desktop")
        .then((p) => setTakeover(p.takeoverWarning ?? null))
        .catch(() => {
          // 预览读不出来（路径不存在等）不影响主界面，静默即可
        });
    },
    5000,
    isDesktop,
  );

  // 密钥库是否锁着（主口令模式尚未解锁）。锁着时**每一次转发都会失败**，
  // 而失败原因藏在运行日志里 —— 必须在最显眼的位置常驻提示，否则用户会以为 Key 配错了。
  // 跟随 5s 轮询：用户可能在设置页解锁后切回来，也可能点了「立即锁定」。
  const [vaultLocked, setVaultLocked] = useState(false);
  usePolling(() => {
    void api
      .getMasterPasswordState()
      .then((s) => setVaultLocked(s.enabled && s.locked))
      .catch(() => {
        // 读不到就不提示（宁可漏提示，也不要因一次 IPC 抖动弹个假警告）
      });
  }, 5000);

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

      {/* 应用内「当前模型」下拉（仅 Codex）：其模型菜单是内置固定清单、拉不到中转模型，
          在此选定实际使用的对外模型名，代理转发时覆盖客户端发来的模型名，即时生效免重启。 */}
      {activeCategory === "codex" && (
        <div className="mx-6 mb-2 rounded-control border border-border bg-surface px-3 py-2.5">
          <div className="mb-1.5 flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-text-secondary">{t("category.activeModel")}</span>
            {activeModel && (
              <button
                type="button"
                onClick={() => void setActiveModel(activeCategory, "")}
                className="text-xs text-text-muted underline underline-offset-2 hover:text-text-secondary"
              >
                {t("category.activeModelAuto")}
              </button>
            )}
          </div>
          <Combobox
            value={activeModel}
            options={discoverable}
            onChange={(v) => void setActiveModel(activeCategory, v)}
            allowCustom={false}
            placeholder={t("category.activeModelAuto")}
            emptyHint={t("category.activeModelEmpty")}
            className="w-full rounded-control border border-border bg-surface-hover px-3 py-1.5 font-mono text-xs text-text-primary outline-none focus:border-primary"
          />
          <p className="mt-1.5 text-[11px] leading-relaxed text-text-muted">{t("category.activeModelHint")}</p>

          {/* 推理强度（方案 A）：Codex 对自定义 provider 不发 reasoning.effort，故在此配默认强度，
              转发时补进请求体（Anthropic 上游映射成 thinking / Chat 上游映射成 reasoning_effort）。
              选「跟随（不注入）」则保持现状不补。 */}
          <div className="mt-3 border-t border-border pt-2.5">
            <span className="text-xs font-medium text-text-secondary">{t("category.effortTitle")}</span>
            <select
              value={activeEffort}
              onChange={(e) => void setActiveEffort(activeCategory, e.target.value)}
              className="mt-1.5 w-full rounded-control border border-border bg-surface-hover px-3 py-1.5 text-xs text-text-primary outline-none focus:border-primary"
            >
              <option value="">{t("category.effort.off")}</option>
              <option value="low">{t("category.effort.low")}</option>
              <option value="medium">{t("category.effort.medium")}</option>
              <option value="high">{t("category.effort.high")}</option>
              <option value="xhigh">{t("category.effort.xhigh")}</option>
            </select>
            <p className="mt-1.5 text-[11px] leading-relaxed text-text-muted">{t("category.effortHint")}</p>
          </div>
        </div>
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

      <div className="flex-1 space-y-2 overflow-y-auto px-6 pb-6">
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
 * 某个 Key 对外「可服务」的模型名集合 —— 必须与后端 `ProviderKey::serviceable_models` 同口径：
 * - 有完整映射 → 只取映射对外名（expectedName），不并入 models 真实名
 * - 无映射 → 取 models 真实名
 * - 已配三档 → 追加 claude-*-4-5 家族代表名
 */
function keyExpectedSet(k: ProviderKey): Set<string> {
  const set = new Set<string>();
  const complete = k.mappings.filter(
    (mp) => mp.expectedName.trim() && mp.realName.trim(),
  );
  if (complete.length > 0) {
    for (const mp of complete) set.add(mp.expectedName.trim());
  } else {
    for (const m of k.models) {
      const n = m.realName.trim();
      if (n) set.add(n);
    }
  }
  if (k.tierOpus?.trim()) set.add("claude-opus-4-5");
  if (k.tierSonnet?.trim()) set.add("claude-sonnet-4-5");
  if (k.tierHaiku?.trim()) set.add("claude-haiku-4-5");
  return set;
}

/**
 * 应用内「当前模型」下拉的候选集 —— 与后端 `discoverable_models`（GET /v1/models）同口径：
 * 主 Key（优先级最高的启用 Key）可服务模型集，与各备用 Key 取交集；空交集时回退主 Key。
 * 保证选中的任意名字在所有候选 Key 都能 resolve、故障转移无感。
 */
function discoverableModels(enabledKeys: ProviderKey[]): string[] {
  const sorted = [...enabledKeys].sort((a, b) => a.priority - b.priority);
  const primary = sorted[0];
  if (!primary) return [];
  const primaryModels = [...keyExpectedSet(primary)];
  const backups = sorted.slice(1).map((k) => keyExpectedSet(k));
  if (backups.length === 0) return primaryModels;
  const intersection = primaryModels.filter((m) => backups.every((s) => s.has(m)));
  // 空交集：对外名不统一，回退主 Key（与后端一致，保证下拉不空且主 Key 一定能路由）
  return intersection.length > 0 ? intersection : primaryModels;
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
