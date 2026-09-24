// 「一键会诊」的纯计算：把当前配置 + Key 列表算成「配好成员与决策者」的补丁。
//
// 抽成纯函数是因为它有**两个**消费者：BrainPage 的「一键配置」按钮（只填、等用户点保存）
// 与 BrainQuickStart 的「一键开始」（填 + 落盘 + 直接进运行面板）。两处各写一份必然漂移
// —— 而本仓已为「同一逻辑两份实现」栽过多次。让计算只有一处，消费者只决定「填完之后做什么」。

import type { BrainConfig, BrainMember, ProviderKey } from "@/types";

/**
 * 能参与聚合、且已拉到模型的 Key。
 *
 * 口径必须含 `allowInAggregate`：「全部禁用 + 勾了允许聚合」正是那个开关设想的典型用法，
 * 只认 `enabled` 会把这类明明可用的 Key 判成「没有可用的 Key」。
 */
export function usableAggregateKeys(keys: ProviderKey[]): ProviderKey[] {
  return keys.filter((k) => (k.enabled || k.allowInAggregate) && k.models.length > 0);
}

/**
 * 计算「一键配置」后要写进 config 的补丁。
 *
 * - 成员：每个可用 Key 取首个模型，**幂等**——已在成员里的跳过，不覆盖用户已调过的选择。
 * - 决策者：未选时默认第一个可用 Key 的首个模型；已选则原样保留。
 * - 无可用 Key 时返回 `null`（调用方据此提示，而不是写一份空配置）。
 *
 * 返回的 `members.length` 是补丁后的**总**成员数（含既有），供「已填充 N 个」文案使用。
 */
export function computeQuickFill(
  config: BrainConfig,
  keys: ProviderKey[],
): Pick<BrainConfig, "members" | "deciderRef" | "enabled"> | null {
  const usable = usableAggregateKeys(keys);
  if (usable.length === 0) return null;

  const existing = new Set(config.members.map((m) => `${m.keyId}::${m.modelName}`));
  const members: BrainMember[] = [...config.members];
  for (const k of usable) {
    const model = k.models[0].realName;
    const ref = `${k.id}::${model}`;
    if (!existing.has(ref)) {
      members.push({ id: `bm_${k.id}_${model}`, keyId: k.id, modelName: model });
      existing.add(ref);
    }
  }
  const deciderRef = config.deciderRef || `${usable[0].id}::${usable[0].models[0].realName}`;
  return { members, deciderRef, enabled: true };
}
