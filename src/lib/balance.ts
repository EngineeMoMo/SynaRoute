/**
 * 余额展示的两个纯函数：金额格式化与「配置指纹」。
 *
 * 抽成独立模块（而不是塞进 utils.ts）是为了让 KeyCard 与 KeyEditor 共用**同一份**
 * 指纹算法 —— 两处各算一套的话，编辑器写回的缓存会被卡片判成「配置已变」立刻重查一次，
 * 每次保存都白发一个请求。
 */
import type { ProviderKey } from "@/types";

/**
 * 把余额数值格式化成给人看的字符串。
 *
 * **绝不能让非零余额显示成 0**：这是本模块唯一的硬判据。固定两位小数看着整齐，
 * 但 `0.0042 USD` 会被渲染成 `0.00` —— 用户会以为额度真用光了，而这正是
 * `docs/17` §2.1 反复强调要避免的那类误导（后端为此让 `remaining` 是 Option
 * 而非 0，前端不能在最后一步把它毁掉）。
 *
 * 故分三档：
 * - 整数 → 千分位、不带小数（`1,024`）；余额常是整数额度或积分
 * - |x| ≥ 0.01 → 两位小数（`84.20`）
 * - 0 < |x| < 0.01 → 两位**有效数字**（`0.0042`），保证不塌成 0
 *
 * locale 固定 `en-US` 而不跟界面语言：这是给数字加千分位，中英文写法一致，
 * 固定下来才能写出稳定的测试判据。
 */
export function formatBalanceAmount(value: number): string {
  // 非有限值（上游给了 null/字符串被 serde 转成 NaN 之类）不猜，如实回退成占位符，
  // 让调用方显示「取不到」而不是编一个数字出来。
  if (!Number.isFinite(value)) return "?";
  if (Number.isInteger(value)) return value.toLocaleString("en-US");
  if (Math.abs(value) >= 0.01) {
    return value.toLocaleString("en-US", {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    });
  }
  // 极小额：按有效数字保精度。maximumSignificantDigits 不会退化成指数记法
  // （toPrecision 会，故此处刻意不用它）。
  return value.toLocaleString("en-US", { maximumSignificantDigits: 2 });
}

/**
 * 已用百分比（0~100 的整数）。`total` 缺失或非正时返回 null —— 不拿假分母凑一个百分比。
 */
export function usedPercent(remaining: number, total: number | undefined): number | null {
  if (total == null || !Number.isFinite(total) || total <= 0) return null;
  if (!Number.isFinite(remaining)) return null;
  const pct = Math.round(((total - remaining) / total) * 100);
  // 上游偶有 remaining > total（赠送额度不计入 total）或负余额，钳到 0~100 免得出现 -13%。
  return Math.min(100, Math.max(0, pct));
}

/**
 * 「这份余额配置」的指纹：只要影响**请求本身**的字段变了，指纹就变。
 *
 * 用途：卡片把指纹与缓存的查询结果一起存。指纹不匹配 = 缓存是旧配置查出来的，
 * 必须重查。没有它就会出现「用户把查询地址改对了、保存后卡片仍显示上次那条 404」——
 * 而这条错误信息正是他刚修掉的东西，看着像改了没生效。
 *
 * 刻意**不**计入的三项：
 * - `enabled`：关掉再打开不代表配置变了，重查一次是白发请求
 * - `template`：纯界面回显（选了哪个预设按钮），不进请求
 * - `autoIntervalMin`：当前无定时任务读它，且它不影响单次查询结果
 *
 * 已知局限：改**密钥明文**不会让指纹变（`ProviderKey` 里只有 `hasSecret`，
 * 前端拿不到密钥本身）。这条被两处兜住 —— 编辑器「测试查询」会在保存密钥后
 * 把结果直接写回缓存，卡片上也永远有手动刷新按钮。
 */
export function balanceFingerprint(key: ProviderKey): string {
  const b = key.balanceQuery;
  if (!b) return "";
  return JSON.stringify([
    // 本 Key 的接口地址：`{{baseUrl}}` / `{{origin}}` 占位符展开后就是它，改了就是换了目标
    key.baseUrl,
    b.url,
    b.method,
    b.auth,
    b.baseUrlOverride ?? "",
    b.apiKeyRef ?? "",
    b.timeoutSecs,
    b.remainingPath ?? "",
    b.accessToken ?? "",
    b.userId ?? "",
  ]);
}
