// 大脑会诊的「一键入口」：把这个产品最独特、也最难被发现的能力做成**可一键发起**的。
//
// # 它补的缺口
//
// 运行面板（`BrainRunPanel`）此前藏在配置页最底部、且**只有 `enabled && deciderRef` 都齐了
// 才出现**。也就是说一个还没配过的用户在这一页看不到任何「开始」的入口 —— 他得先找到成员卡里
// 的「一键配置」、再滚到底部点保存、再往下滚才看到运行面板。会诊是本产品的招牌，却是隐形的。
//
// 现在：未就绪时给一张显眼的「一键配置并开始」卡（配置 → **落盘** → 就地展开运行面板），
// 就绪时它本身就是运行面板。两种状态共用一处挂载点。
//
// # 为什么「开始」必须自己落盘，不能只 quickFill
//
// `run_plan` / `run_preview` / `run_write` 读的是**存储里**的聚合配置，不是前端内存那份。
// quickFill 只改内存（等用户点保存）。所以「一键开始」若只 quickFill 就发起会诊，后端拿到的
// 是旧的/空的配置 —— 成员和决策者根本没进去。故这里 `computeQuickFill → saveBrainConfig →
// onReady`，保存成功之后才翻成就绪、露出运行面板。

import { useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Card, CardContent } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { BrainRunPanel } from "@/components/BrainRunPanel";
import { computeQuickFill, usableAggregateKeys } from "@/lib/brainQuickStart";
import type { BrainConfig, CategoryType, ProviderKey } from "@/types";
import { Sparkles, Users } from "lucide-react";

interface Props {
  category: CategoryType;
  config: BrainConfig;
  keys: ProviderKey[];
  /** 保存成功后把新配置回传给父页，由它 setConfig → 就绪 → 露出运行面板。 */
  onReady: (config: BrainConfig) => void;
}

export function BrainQuickStart({ category, config, keys, onReady }: Props) {
  const t = useT();
  const showToast = useStore((s) => s.showToast);
  const [busy, setBusy] = useState(false);

  // 就绪（已开启 + 已选决策者）就直接是运行面板 —— 不再重复挂一份，行为与原先一致。
  if (config.enabled && config.deciderRef) {
    return <BrainRunPanel category={category} />;
  }

  const usable = usableAggregateKeys(keys);

  const start = async () => {
    const patch = computeQuickFill(config, keys);
    if (!patch) {
      showToast("error", t("brain.quickFillNoKeys"));
      return;
    }
    setBusy(true);
    try {
      // 先落盘、成功后才 onReady —— 后端 run_* 读的是存储里的配置（见文件头）。
      const next = { ...config, ...patch };
      await api.saveBrainConfig(next);
      onReady(next);
      showToast("success", t("brain.quickFillDone", { n: next.members.length }));
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="border-primary/30 bg-primary/[0.04]">
      <CardContent className="space-y-3 pt-4">
        <div className="flex items-center gap-2">
          <Sparkles size={18} className="shrink-0 text-primary" />
          <div className="text-sm font-semibold text-text-primary">{t("brain.consultTitle")}</div>
        </div>
        <p className="text-xs leading-relaxed text-text-secondary">{t("brain.consultDesc")}</p>

        {usable.length === 0 ? (
          // 一个可用 Key 都没有：给按钮点了也只会报错，不如直接说清楚该去哪补。
          <div className="rounded-control border border-border bg-background p-2.5 text-xs text-text-secondary">
            {t("brain.quickFillNoKeys")}
          </div>
        ) : (
          <>
            <div className="flex items-center gap-1.5 text-[11px] leading-relaxed text-text-muted">
              <Users size={12} className="shrink-0" />
              {/* 如实标注代价：会同时打 N 个上游、每个都烧额度，比单次调用慢。别把招牌功能
                  说得像免费午餐 —— 事先说清比事后被账单吓到便宜。 */}
              {t("brain.consultCost", { n: usable.length })}
            </div>
            <Button size="sm" disabled={busy} onClick={() => void start()}>
              <Sparkles size={14} /> {busy ? t("brain.consultStarting") : t("brain.consultStart")}
            </Button>
          </>
        )}
      </CardContent>
    </Card>
  );
}
