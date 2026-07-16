import { useMemo } from "react";
import { useStore } from "@/store";
import { KeyCard } from "@/components/KeyCard";
import { ProxyStatusBar } from "@/components/ProxyStatusBar";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import type { ProviderKey } from "@/types";
import { Plus, AlertTriangle, Inbox } from "lucide-react";

/** 分类主页：代理状态条 + 模型映射兜底提示 + Key 卡片列表 */
export function CategoryPage({ onAddKey, onEditKey }: {
  onAddKey: () => void;
  onEditKey: (k: ProviderKey) => void;
}) {
  const { activeCategory, keys, proxy, loading } = useStore();
  const t = useT();

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

      {/* 映射缺口提示（FR-006a） */}
      {gaps.length > 0 && (
        <div className="mx-6 mb-2 flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-xs text-warning">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <div>
            <span className="font-medium">{t("category.mappingGapTitle")}</span>
            {gaps.map((g) => (
              <div key={g.expected} className="mt-0.5">
                {t("category.mappingGapItem", { expected: g.expected, keys: g.missingKeys.join("、") })}
              </div>
            ))}
          </div>
        </div>
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

/**
 * 模型映射兜底检查（FR-006a）
 * 思路：收集所有启用 Key "对外可服务的期望模型名"（真实模型名 + 映射后的期望名）。
 * 对每个期望名，找出哪些启用 Key 无法提供它 —— 这些就是切换时的缺口。
 */
function detectMappingGaps(enabledKeys: ProviderKey[]): Gap[] {
  if (enabledKeys.length < 2) return [];

  // 每个 Key 能对外服务的期望模型集合
  const keyExpectedSets = enabledKeys.map((k) => {
    const set = new Set<string>();
    // 无映射时，真实模型名即期望名
    for (const m of k.models) set.add(m.realName);
    // 有映射时，映射的期望名可服务
    for (const mp of k.mappings) set.add(mp.expectedName);
    return { key: k, set };
  });

  // 全局期望模型名（并集）
  const allExpected = new Set<string>();
  keyExpectedSets.forEach(({ set }) => set.forEach((e) => allExpected.add(e)));

  const gaps: Gap[] = [];
  for (const expected of allExpected) {
    const missing = keyExpectedSets
      .filter(({ set }) => !set.has(expected))
      .map(({ key }) => key.name);
    // 只有"部分 Key 有、部分没有"才算缺口（全没有的不是候选期望名）
    if (missing.length > 0 && missing.length < enabledKeys.length) {
      gaps.push({ expected, missingKeys: missing });
    }
  }
  return gaps;
}
