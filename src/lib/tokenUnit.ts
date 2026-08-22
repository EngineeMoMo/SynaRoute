/**
 * 上下文窗口输入的「数值 + 单位」精确换算（从 KeyEditor 抽出，便于单测 —— 审计 A2-04）。
 *
 * 核心纪律：**十进制进、十进制出，全程 BigInt，绝不碰二进制浮点**。
 * KeyEditor 的上下文窗口框让用户填「200」选「K」得 200000 token；反过来已有 token
 * 数要无损回显成「数值 + 单位」。中间任何一步用 `Number` 乘除，都会在
 * `1.000001 * 1_000_000 === 1000000.9999999999` 这类地方把合法值判错或显示成一串小数。
 */

/** 单位下拉的三档：token 原始数值 / 千（×1000）/ 百万（×1000000）。 */
export type TokenUnit = "token" | "K" | "M";

export const UNIT_MULTIPLIER: Record<TokenUnit, number> = { token: 1, K: 1_000, M: 1_000_000 };
export const UNIT_DECIMALS: Record<TokenUnit, number> = { token: 0, K: 3, M: 6 };

/** 后端 `ModelInfo.context_window` 是 `Option<u32>`，前端必须同上限，不能等 IPC 反序列化才失败。 */
export const MAX_CONTEXT_TOKENS = 0xffff_ffff;

/**
 * 精确地把十进制数值 + 单位换成整数 token，避免二进制浮点误差。
 *
 * 例如 JS 的 `1.000001 * 1_000_000` 实际是 `1000000.9999999999`；用 Number 乘完再
 * `isInteger` 会把合法的 1,000,001 token 判错。这里直接按十进制位数补零，绝不猜值。
 *
 * 返回 `null` 的情形（都刻意不做四舍五入，宁可拒绝）：
 * - 格式非法（负号、多个小数点、字母、空串）
 * - 小数位超过该单位精度（会产生半个 token）
 * - 结果 ≤ 0 或超过 u32 上限
 */
export function tokensFromAmount(raw: string, unit: TokenUnit): number | null {
  const matched = /^(\d+)(?:\.(\d+))?$/.exec(raw.trim());
  if (!matched) return null;

  const whole = matched[1];
  const fraction = (matched[2] ?? "").replace(/0+$/, "");
  const decimals = UNIT_DECIMALS[unit];
  if (fraction.length > decimals) return null; // 该精度会产生半个 token，拒绝而非四舍五入

  const tokens =
    BigInt(whole) * BigInt(UNIT_MULTIPLIER[unit]) +
    BigInt((fraction + "0".repeat(decimals - fraction.length)) || "0");
  if (tokens <= 0n || tokens > BigInt(MAX_CONTEXT_TOKENS)) return null;
  return Number(tokens);
}

/** 为已有 token 数选择初始单位：按数量级选最大单位，允许精确小数回显。 */
export function preferredUnit(tokens: number | undefined): TokenUnit {
  if (tokens === undefined) return "K"; // 上下文窗口通常是数十万，空值默认 K，直接填 200 即 200K
  if (tokens >= 1_000_000) return "M";
  if (tokens >= 1_000) return "K";
  return "token";
}

/**
 * 人类可读的短写（`128000` → `128K`、`1000000` → `1M`、`8192` → `8192`）。
 *
 * 只用于**展示**（如「自动 128K」这类占位提示），不参与任何换算 —— 故复用
 * `preferredUnit` + `amountForUnit` 那套无损逻辑，而不是随手写个 `/1000`：
 * 后者会把 8192 显示成 `8.192K`，读起来比原数还费劲。
 */
export function fmtTokenShort(tokens: number): string {
  const unit = preferredUnit(tokens);
  const amount = amountForUnit(tokens, unit);
  // 带小数就说明这个数不是该单位的整数倍（如 8192 在 K 下是 8.192），
  // 那时原样报 token 数更好读。
  if (amount.includes(".")) return String(tokens);
  return unit === "token" ? amount : `${amount}${unit}`;
}

/**
 * 按当前单位**无损**反算显示值。
 *
 * 不能用 `String(tokens / multiplier)`：接近 `Number.MAX_SAFE_INTEGER` 时二进制浮点
 * 会把 `9007199254740991 / 1000` 显示成 `9007199254740.99`，再解析就少 1 token。
 * 这里用 BigInt 做商余数、手工插十进制点，确保 `tokensFromAmount(amountForUnit(...))`
 * 对整个安全整数范围都严格往返。
 */
export function amountForUnit(tokens: number | undefined, unit: TokenUnit): string {
  if (tokens === undefined) return "";
  const divisor = BigInt(UNIT_MULTIPLIER[unit]);
  const raw = BigInt(tokens);
  const whole = raw / divisor;
  const remainder = raw % divisor;
  if (remainder === 0n) return String(whole);

  const decimals = UNIT_DECIMALS[unit];
  const fraction = remainder.toString().padStart(decimals, "0").replace(/0+$/, "");
  return `${whole}.${fraction}`;
}
