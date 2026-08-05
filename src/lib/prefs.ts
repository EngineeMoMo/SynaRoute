import type { AppSettings } from "@/types";

/**
 * **前端可写的那部分设置**（`saveSettings` 的入参类型）。
 *
 * 与后端 `model.rs` 的 `UserPrefs` 一一对应。后端自管字段（粘滞端口、MCP 注册记录、
 * 已选模型、密钥库模式镜像、开机自启动、首启向导标记）**不在这里** ——
 * 前端在类型上就无法表达「我要改它们」。
 *
 * 用 `Pick<AppSettings, ...>` 派生而不是重新声明字段类型：那样 `theme` 的
 * `ThemePref` 之类的具体类型永远与 `AppSettings` 保持一致，不会因为哪天改了枚举
 * 而两边分叉。要加/减字段只需改这个键名列表。
 */
export type UserPrefs = Pick<
  AppSettings,
  | "theme"
  | "language"
  | "lanExposure"
  | "requestLogEnabled"
  | "logDownstreamRawEnabled"
  | "healthCheckIntervalSecs"
  | "failoverTotalBudgetMs"
  | "logDir"
  | "upstreamRetryEnabled"
  | "healthProbeRealCompletion"
  | "healthProbeTestMessages"
  | "aggregateTraceEnabled"
  | "trayModelSwitchEnabled"
>;

/**
 * 从一份完整设置里挑出用户偏好。
 *
 * **必须逐字段列举，禁止用解构 rest**（`const { autoStart, ...rest } = s`）：
 * 那样日后给后端加自管字段时会**默认漏进** rest 被发出去，重蹈黑名单的覆辙 ——
 * 而黑名单正是本次要消灭的东西。逐字段写虽然啰嗦，但「加字段默认不发」是结构性保证。
 */
export function pickPrefs(s: AppSettings): UserPrefs {
  return {
    theme: s.theme,
    language: s.language,
    lanExposure: s.lanExposure,
    requestLogEnabled: s.requestLogEnabled,
    logDownstreamRawEnabled: s.logDownstreamRawEnabled,
    healthCheckIntervalSecs: s.healthCheckIntervalSecs,
    failoverTotalBudgetMs: s.failoverTotalBudgetMs,
    logDir: s.logDir,
    upstreamRetryEnabled: s.upstreamRetryEnabled,
    healthProbeRealCompletion: s.healthProbeRealCompletion,
    healthProbeTestMessages: s.healthProbeTestMessages,
    aggregateTraceEnabled: s.aggregateTraceEnabled,
    trayModelSwitchEnabled: s.trayModelSwitchEnabled,
  };
}
