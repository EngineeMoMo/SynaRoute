import { useEffect, useMemo, useState } from "react";
import { useStore } from "@/store";
import { KeyCard } from "@/components/KeyCard";
import { ProxyStatusBar } from "@/components/ProxyStatusBar";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import type { ProviderKey } from "@/types";
import { Plus, AlertTriangle, Inbox, X } from "lucide-react";

/** 分类主页：代理状态条 + 模型映射兜底提示 + Key 卡片列表 */
export function CategoryPage({ onAddKey, onEditKey }: {
  onAddKey: () => void;
  onEditKey: (k: ProviderKey) => void;
}) {
  const { activeCategory, keys, proxy, loading, refreshCategory } = useStore();
  const t = useT();
  const [gapDialogOpen, setGapDialogOpen] = useState(false);

  useEffect(() => {
    const id = setInterval(() => void refreshCategory(), 5000);
    return () => clearInterval(id);
  }, [refreshCategory]);

  // 排序：按优先级
  const sorted = useMemo(
    () => [...keys].sort((a, b) => a.priority - b.priority),
    [keys]
  );

  // 模型映射兜底检查（FR-006a）：统计启用 Key 中，各期望模型的覆盖缺口
  const gaps = useMemo(() => detectMappingGaps(keys.filter((k) => k.enabled)), [keys]);

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
            <Button variant="secondary" onClick={onAddKey}>
              <Plus size={16} /> {t("category.addFirst")}
            </Button>
          </div>
        )}

        {!loading &&
          sorted.map((k) => <KeyCard key={k.id} k={k} onEdit={onEditKey} />)}
      </div>
    </div>
  );
}

interface Gap {
  expected: string;
  missingKeys: string[];
}

/** 某个 Key 对外「可服务」的模型名集合（与后端 ProviderKey::serviceable_models 对齐）：
 *  映射的对外名（expectedName）+ 原生真实模型名（realName）。 */
function keyExpectedSet(k: ProviderKey): Set<string> {
  const set = new Set<string>();
  for (const mp of k.mappings) set.add(mp.expectedName);
  for (const m of k.models) set.add(m.realName);
  return set;
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
