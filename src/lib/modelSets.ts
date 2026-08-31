import type { ProviderKey } from "@/types";

/**
 * 某个 Key 对外「可服务」的模型名集合 —— 必须与后端 `ProviderKey::serviceable_models` 同口径：
 * - 有完整映射 → 只取映射对外名（expectedName），不并入 models 真实名
 * - 无映射 → 取 models 真实名
 * - 已配三档 → 追加 claude-*-4-5 家族代表名
 *
 * 从 CategoryPage 搬到这里，是因为状态条（ProxyStatusBar）也要算「当前模型」的候选集，
 * 而它与分类页是两个组件。**两份实现必然漂移**，而漂移的后果是：界面列出的模型后端压根
 * 没宣称过，于是转发时被 `reject_if_unserviceable` 当成「客户端自己编的名字」放过、
 * 静默降级到别的模型 —— 用户选了 A 拿到 B 的回答，日志里也只是一行正常的「兜底改写」。
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
 * 应用内「当前模型」下拉的候选集 —— 必须与后端 `model_pool::discoverable_models`
 * （即 `GET /v1/models`）同口径：按 priority 升序，把各 Key 的可服务集**并**起来，去重保序。
 *
 * 2026-08-31 起是并集（此前是交集，空交集时回退主 Key）。交集把备用 Key 独有的模型整个
 * 藏了起来，而它们明明可用 —— 只要请求真的被路由到那条 Key，而这由后端
 * `model_pool::rank_candidates` 保证（排序键把「能原生服务本次模型」摆在 priority 之前）。
 *
 * 🔴 顺序是契约，不只是观感：首个会被写进 `env.ANTHROPIC_MODEL`、Codex 目录挑默认模型、
 * 桌面端选择器的默认项。故必须是「主 Key 的全部（保其自身顺序）→ 再按 priority 依次
 * 追加各备用 Key 独有的」。跨语言一致性由 tests/discoverableModelsParity.test.ts 钉住。
 */
export function discoverableModels(enabledKeys: ProviderKey[]): string[] {
  const out: string[] = [];
  for (const k of [...enabledKeys].sort((a, b) => a.priority - b.priority)) {
    // Set 的迭代顺序 = 插入顺序，与后端 serviceable_models 返回的 Vec 顺序一致。
    for (const m of keyExpectedSet(k)) if (!out.includes(m)) out.push(m);
  }
  return out;
}

/**
 * 当前**路由意义上**的主 Key —— 即「按 priority 升序的第一个**启用** Key」。
 *
 * ⚠️ 口径要点：**不是 `priority === 0` 那条**。后端路由与托盘都用这个口径
 * （`Store::enabled_keys_sorted`；托盘子菜单只列启用的，因为把禁用 Key 设为主毫无意义
 * ——它根本不进候选池）。若 priority 为 0 的那条被禁用了，真正先被使用的是下一条。
 *
 * 全部展示位现已统一到这个口径：状态条、托盘、以及 `KeyCard` 的「主 Key」徽标
 * （由 `CategoryPage` 算好 `isRoutingPrimary` 传进去 —— 单张卡片看不到整列，
 * 无法自行判断）。此前 KeyCard 用 `priority === 0`，在两种真实场景下与实际路由不符：
 * ① priority-0 被禁用 → 徽标指着一条不进候选池的 Key；
 * ② 多条同为 0（历史配置 / cc-switch 导入曾把 sort_index 照搬成全 0）→ 多张卡片
 *    同时显示「主 Key」，且一个「设为主」按钮都没有。
 */
export function routingPrimaryKey(keys: ProviderKey[]): ProviderKey | undefined {
  return [...keys].filter((k) => k.enabled).sort((a, b) => a.priority - b.priority)[0];
}
