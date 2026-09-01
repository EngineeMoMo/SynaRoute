//! 日志正文的体积上限与截断口径。
//!
//! `#[path]` 挂在 [`super`] 下（同 `lan_guard`/`log_rotate`/`route_meta` 的理由）：
//! `proxy.rs` 棘轮冻结在 3036、余量为 0，而这一族是自洽的 —— 只回答「一段文本进日志时
//! 留多少、留哪一段」，不碰转发、不碰记账。
//!
//! # 🔴 为什么截断要**头尾各留**，而不是只留头部
//!
//! 2026-09-01 排查一条上游 400 时实测到的：上游报
//! `mcp__codex_app__automation_update: tool parameter root must be an object type
//! (root schema is an anyOf/oneOf union with a non-object branch)`
//! —— 它**点名了**某个工具的 schema，而排这类 400 唯一的办法就是核对「我们究竟发了什么」。
//!
//! 那次请求体 **301074 字符**，日志里留下前 20019 个，`tools` 段整段落在被截掉的
//! 281055 字符里。也就是说：上游明确告诉我们错在哪，而我们的日志恰好不含那一段。
//!
//! 这不是运气不好。JSON 对象的字段顺序决定了它是**系统性**的：`model`/`messages` 在前，
//! 而 `tools`/`tool_choice`/`response_format` 这些**最常导致 400** 的字段都在末尾。
//! 只留头部 = 系统性地丢掉最可能的成因，而留下的那部分（对话正文）恰恰最少出问题。
//!
//! # 边界：这里只管「留多少」，不管脱敏
//!
//! 脱敏在写入侧（`RequestTrace` 的构造点与 `diagnostics::redact_config_secrets`）。
//! 本模块不看内容语义 —— 加了就会变成第二处真相，而两处必然漂移。

/// 请求/响应体在日志里的最大字符数（防止超大 body 撑爆内存日志）。
///
/// 🔴 **2026-09-01 从 20000 提到 65536**：实测一次大脑聚合的请求体（含 4 个工具 schema +
/// 8 个文件内容）长度 **31662 字节**，20K 截断让 `tools` 数组只留下前 1.5 个、后三个工具的
/// schema 完全丢失；而上游报 400 时常**点名工具**（`mcp__codex_app__automation_update:
/// tool parameter root must be an object type`），没有完整 schema 就无从核对「我们究竟发了什么」。
/// <br>65536 足够覆盖「多模态 + 多工具」这类正常大请求（头尾各留模式下头段 48K、尾段 16K），
/// 同时仍对**真正异常大**的（如误传整个仓库）保留截断保护。
pub(super) const REQ_LOG_CAP: usize = 65536;

/// 按默认上限截断。
pub(super) fn cap(s: &str) -> String {
    cap_to(s, REQ_LOG_CAP)
}

/// 按指定上限截断，**头尾各留一段**（供双段日志分别控额度）。
///
/// 头 3/4、尾 1/4：尾段只要够放下 `tools` 数组的末几项与那几个顶层字段就行，
/// 而对话正文（在头段）仍是判断「协议转换有没有出错」的主要依据。理由见模块头。
///
/// `limit` 为 0 时退化成「只给省略提示」，不 panic（`skip(total)` 合法、产出空串）。
pub(super) fn cap_to(s: &str, limit: usize) -> String {
    let total = s.chars().count();
    if total <= limit {
        return s.to_string();
    }
    let tail_len = limit / 4;
    let head_len = limit - tail_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s.chars().skip(total - tail_len).collect();
    format!(
        "{head}\n…（中间已省略，共 {total} 字符；下面是结尾 {tail_len} 字符，\
         含 tools / response_format 等易致 400 的字段）…\n{tail}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 本模块存在的全部理由：**尾段必须留住**。
    ///
    /// 用 2026-09-01 那次的真实形态做夹具（正文很长、`tools` 在末尾）。
    /// 注入「改回只留头部」时这条必须变红。
    #[test]
    fn the_tail_survives_truncation_because_tools_lives_there() {
        let body = format!(
            "{{\"model\":\"grok-4.6\",\"messages\":[{}],\"tools\":[{{\"name\":\"automation_update\"}}]}}",
            "\"x\",".repeat(22000)  // 从 9000 提到 22000，确保超过新上限 65536
        );
        assert!(body.chars().count() > REQ_LOG_CAP, "夹具前提：必须真的超过上限");
        let out = cap_to(&body, REQ_LOG_CAP);
        assert!(
            out.contains("automation_update"),
            "上游点名的工具名在尾段，截断后必须还看得到 —— 否则那条 400 无从核对"
        );
        assert!(out.contains("\"model\":\"grok-4.6\""), "头段也不能丢：那是请求的身份");
        assert!(out.contains("共 "), "要如实报出总长度，否则读者不知道省了多少");
    }

    /// 不超上限时**一个字符都不许动** —— 绝大多数请求走这条路。
    #[test]
    fn short_bodies_pass_through_untouched() {
        assert_eq!(cap_to("hello", 100), "hello");
        let exact = "y".repeat(REQ_LOG_CAP);
        assert_eq!(cap(&exact), exact, "正好等于上限时不该被截");
    }

    /// 截断后的长度必须**有界**（否则这个上限就是白设的）。
    #[test]
    fn truncated_output_stays_bounded() {
        let huge = "z".repeat(500_000);
        let out = cap(&huge);
        // 头 + 尾 + 一行提示：提示是常量级，给 200 字符余量足够。
        assert!(
            out.chars().count() < REQ_LOG_CAP + 200,
            "截断结果不该随输入增长：{}",
            out.chars().count()
        );
    }

    /// 边界：`limit == 0` 不许 panic（`skip(total)` 合法）。
    #[test]
    fn zero_limit_does_not_panic() {
        let out = cap_to("abc", 0);
        assert!(out.contains("共 3 字符"), "只剩省略提示也要说清总长：{out}");
    }
}
