import type { ProviderKey } from "@/types";

/**
 * 某个 Key 对外「可服务」的模型名集合 —— 必须与后端 `ProviderKey::serviceable_models` 同口径：
 * - 有完整映射 → 只取映射对外名（expectedName），不并入 models 真实名
 * - 无映射 → 取 models 真实名
 * - 已配三档 → 追加 claude-*-4-5 家族代表名
 *
 * 从 CategoryPage 搬到这里，是因为状态条（ProxyStatusBar）也要算「当前模型」的候选集，
 * 而它与分类页是两个组件。**两份实现必然漂移**，而漂移的后果是：状态条列出的模型
 * 在某个备用 Key 上其实路由不了，故障转移落到它时就报「模型不存在」。
 */
export function keyExpectedSet(k: ProviderKey): Set<string> {
  const set = new Set<string>();
  const complete = k.mappings.filter((mp) => mp.expectedName.trim() && mp.realName.trim());
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
export function discoverableModels(enabledKeys: ProviderKey[]): string[] {
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
 * 当前**路由意义上**的主 Key —— 即「按 priority 升序的第一个**启用** Key」。
 *
 * ⚠️ 口径要点：**不是 `priority === 0` 那条**。后端路由与托盘都用这个口径
 * （`Store::enabled_keys_sorted`；托盘子菜单只列启用的，因为把禁用 Key 设为主毫无意义
 * ——它根本不进候选池）。若 priority 为 0 的那条被禁用了，真正先被使用的是下一条。
 *
 * 注意 `KeyCard` 目前仍用 `priority === 0` 画「主」徽标，所以在
 * 「priority-0 被禁用」这个场景下，列表徽标与状态条会指向不同的 Key。
 * 改 KeyCard 要连带调整「设为主」按钮的显隐规则，属独立议题，不在本次改动范围内。
 */
export function routingPrimaryKey(keys: ProviderKey[]): ProviderKey | undefined {
  return [...keys].filter((k) => k.enabled).sort((a, b) => a.priority - b.priority)[0];
}
