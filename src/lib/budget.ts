/**
 * 花费预算是否可用（空串 = 未设，合法 = 不限）。
 *
 * # 为什么前端也要判（同 `isValidCostMultiplier`）
 *
 * 后端 `usage_cost::budget_status` 对无效值**静默视同「未设预算」**。那个兜底是对的
 * （不能让一个笔误把每条 Key 都标成超支），但它意味着用户填错时界面不会自己说 ——
 * 他填了 `0` / `-5` / `abc` 以为设了上限，实际不生效且无人告知。故这里就地标红。
 *
 * # 判据与后端逐条对齐
 *
 * 正数、有限即可（后端 `budget_status` 只要求 `> 0.0 && is_finite`，没有上界 ——
 * 一个大得离谱的预算是无害的，永远不会触发）。`"inf"` / `"1e400"` 会被 `Number()` 解析成
 * `Infinity` 且 `> 0` 为真，必须靠 `Number.isFinite` 显式挡掉（后端用 `is_finite()` 同口径）。
 *
 * 从 KeyEditor 搬出来的理由同 `costMultiplier`：它是一条跨语言不变量（与 Rust 同口径），
 * 不是组件的私事 —— 放组件里就只能靠渲染整个抽屉来验，等于没测。
 */
export function isValidBudget(raw: string): boolean {
  const s = raw.trim();
  if (!s) return true; // 未填 = 不限
  const n = Number(s);
  return Number.isFinite(n) && n > 0;
}

/**
 * 解析成美元数（空 / 非法 → undefined，即「不限」）。
 *
 * `buildDraftKey` 落库前用它归一。返回 `undefined`（而非 0）很关键：
 * 后端 `budget_usd` 是 `Option<f64>`，`Some(0.0)` 会被 `budget_status` 当无效丢弃，
 * 但让一个笔误落成 `0` 再靠后端兜是脆的 —— 在入口就归一成「没有预算」最干净。
 */
export function parseBudget(raw: string): number | undefined {
  const s = raw.trim();
  if (!s) return undefined;
  const n = Number(s);
  return Number.isFinite(n) && n > 0 ? n : undefined;
}
