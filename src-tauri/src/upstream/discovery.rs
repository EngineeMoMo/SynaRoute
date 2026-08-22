//! 模型发现：向上游拉 /models 并解析出模型名。
//!
//! 与 probe 共用 endpoint::model_endpoints —— 「候选端点怎么排」这条判据只有一处。

use crate::error::{AppError, AppResult};
use crate::model::ProviderKey;
use serde_json::Value;

use super::client::{apply_models_auth, build_client, fast_timeout};
use super::endpoint::model_endpoints;

/// 拉取模型列表（FR-004）。返回真实模型名数组。
pub async fn fetch_models(key: &ProviderKey, secret: &str) -> AppResult<Vec<String>> {
    let client = build_client(key)?;

    // 不同厂商的模型端点路径不一致（DeepSeek 等第三方对 /v1/models 返回 404），
    // 依次尝试候选路径，任一 2xx 即用；全失败则汇总错误。
    let mut last_err = String::from("无候选端点");
    // **最有信息量的那条**失败，优先于「最后一条」上报。见 more_informative_models_error。
    //
    // 真机（2026-08-22 agentrouter.org）：`/v1/models` 回 401
    // `unauthorized client detected`（真因），随后 `/models` 拿到站点首页 HTML。
    // 只报最后一条时用户看到的是「返回的是网页，请确认 Base URL 是否以 /v1 结尾」——
    // 而 Base URL 本来就是对的，方向完全错。
    let mut best_err: Option<String> = None;
    for url in model_endpoints(&key.base_url) {
        let mut req = client.get(&url).timeout(fast_timeout(key));
        // /models 用双鉴权头（Bearer + x-api-key），兼容把 Anthropic 挂子路径的 OpenAI 风格 /models
        req = apply_models_auth(req, secret);
        // 客户端身份头（UA 等）由 build_client 装成默认头，这里不再逐请求补 ——
        // 结构上保证不会漏（本项目在这件事上已踩过三次，见 build_client 的文档）。

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                // 连接层失败：区分「域名解析不了/连不上」与「超时」，并直接给出该查什么。
                // 原先只有 `e.to_string()`（`error sending request for url (…)`），
                // 用户看不出是自己网络问题、base_url 写错、还是上游真的挂了。
                last_err = if e.is_timeout() {
                    format!("连接超时（{url}）：上游无响应，可能是网络受限或该站点当前不可用")
                } else if e.is_connect() {
                    format!(
                        "连不上 {url}：请检查 Base URL 是否写错、域名能否解析、\
                         以及是否需要代理/VPN 才能访问该站点"
                    )
                } else {
                    format!("请求 {url} 失败：{e}")
                };
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            // 带上状态码的可行动含义：401/403 是密钥问题，404 是路径问题（会自动换下个候选）。
            let hint = match status.as_u16() {
                401 | 403 => "（密钥无效或无权限，请检查密钥是否填对、是否已过期）",
                404 | 405 => "（该路径不存在，将自动尝试其他候选路径）",
                429 => "（被限流，稍后再试）",
                s if s >= 500 => "（上游服务异常，与本地配置无关）",
                _ => "",
            };
            // **把上游自己那句话带出来**：真机上 agentrouter.org 回的是
            // `unauthorized client detected`（客户端准入，不是密钥问题），
            // 而我们的 hint 说「请检查密钥是否填对」——方向相反。上游原文才是真因所在。
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.trim().chars().take(200).collect();
            last_err = if snippet.is_empty() {
                format!("HTTP {status} @ {url}{hint}")
            } else {
                format!("HTTP {status} @ {url}{hint}：{snippet}")
            };
            best_err = more_informative_models_error(best_err, &last_err, status.as_u16());
            // 404/405 说明路径不对，换下一个候选；其他状态码同样重试下一个
            continue;
        }
        // 响应体必须是 JSON。**不能直接 `resp.json().await?`**：上游返回 HTML 错误页/登录页时，
        // serde 只会抛出 `expected value at line 1 column 1`（实测用户就是被这句卡住的）——
        // 那是「第 1 行第 1 列不是合法 JSON」的字面意思，对用户毫无指向性。
        // 这里先取文本再解析，失败时报出「拿到的不是 JSON」并附开头片段，让用户一眼看出
        // 究竟是被挡在了登录页、还是 base_url 少了 `/v1`。
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                last_err = format!("读取 {url} 的响应失败：{e}");
                continue;
            }
        };
        let body: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                last_err = non_json_models_hint(&url, &text);
                continue;
            }
        };
        let names = parse_model_names(&body);
        if !names.is_empty() {
            return Ok(names);
        }
        last_err = format!("{url} 返回了 JSON 但其中没有模型列表（该站点可能不提供模型查询接口）");
    }
    // 末尾统一附「可手动录入」的出路：这条链路失败不阻塞用户继续配置。
    // 优先报**最有信息量**的那条，而不是最后一条（见 best_err 的声明处）。
    Err(AppError::upstream_msg(format!(
        "拉取模型失败：{}",
        best_err.unwrap_or(last_err)
    )))
}

/// 在多个候选端点的失败里挑「更有信息量」的那条。
///
/// ## 为什么需要它
///
/// 候选链会按序试 2~4 个 URL，最终只能报一条。报最后一条会系统性地把用户引向**最不可能**
/// 的那个原因：真机 agentrouter.org 的实际序列是
///
/// 1. `/v1/models` → `401 unauthorized client detected`  ← **真因**（客户端准入）
/// 2. `/models`    → `200` 但返回站点首页 HTML
///
/// 于是界面上写的是「返回的是网页，请确认 Base URL 通常以 /v1 结尾」，而 Base URL
/// 本来就是对的 —— 用户照着改只会越改越错。
///
/// ## 判据：越「具体地拒绝了你」越有信息量
///
/// - `401/403`（明确拒绝，且带上游原话）> 其它 4xx/5xx > `404/405`（只说明这条路径不存在）
///   > 非 JSON / HTML（说明这个地址压根不是接口）。
/// - 同级别时保留**先出现**的那条（候选顺序本身就是按「最可能是对的」排的）。
///
/// `status` 为 `None` 表示不是 HTTP 层失败（连接失败 / 非 JSON 响应），排在最低级。
fn more_informative_models_error(
    current: Option<String>,
    candidate: &str,
    status: u16,
) -> Option<String> {
    fn rank(status: u16) -> u8 {
        match status {
            401 | 403 => 3, // 明确拒绝了你，且通常带原因
            404 | 405 => 1, // 只说明这条路径不存在，最没信息量
            0 => 0,         // 连接失败 / 非 JSON：连是不是接口都不确定
            _ => 2,
        }
    }
    match &current {
        // 同级别保留先出现的那条：候选顺序即「最可能是对的」的顺序。
        Some(existing) => {
            let keep = rank(models_error_status(existing)) >= rank(status);
            if keep {
                current
            } else {
                Some(candidate.to_string())
            }
        }
        None => Some(candidate.to_string()),
    }
}

/// 从已记下的错误串里反读状态码（`HTTP 401 @ …` 形态），读不出按 0（最低级）。
///
/// 之所以从文本反读而不是并存一个数字：这两者只在本函数内配对使用，
/// 多存一个字段就多一处可能与文本不同步的地方；而格式由上面唯一一处 `format!` 产生。
fn models_error_status(err: &str) -> u16 {
    err.strip_prefix("HTTP ")
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// 拉取模型时上游返回**非 JSON** 的可行动提示。
///
/// 拆成纯函数是为了**可验证**：这条判据的价值全在措辞上，而端到端跑一次拉取需要真实上游。
///
/// 真机背景（2026-08-02）：用户配了一个中转商，拉取模型时只看到
/// `error decoding response body ← expected value at line 1 column 1`。
/// 那是 serde 在说「第 1 行第 1 列不是合法 JSON」，对用户零指向性 —— 实际原因是该站点
/// 把请求挡在了 HTML 页面上。提示必须说清「拿到的是网页」以及「该去改 Base URL」。
pub(super) fn non_json_models_hint(url: &str, text: &str) -> String {
    let head: String = text.trim().chars().take(80).collect();
    let low = head.to_ascii_lowercase();
    if low.starts_with("<!doctype") || low.starts_with("<html") || low.starts_with("<?xml") {
        format!(
            "{url} 返回的是网页而不是 JSON（可能被挡在登录页/防护页，或 Base URL 指向了站点首页）。\
             请确认 Base URL 填的是接口地址（通常以 /v1 结尾）"
        )
    } else if head.is_empty() {
        format!("{url} 返回了空响应（该地址可能不是模型查询接口）")
    } else {
        format!("{url} 返回的内容不是合法 JSON，开头是「{head}」")
    }
}

/// 从模型列表响应解析模型名，兼容多种结构：
/// - OpenAI/Anthropic: `{data:[{id}]}`
/// - 部分厂商: `{models:[{id/name}]}` 或顶层数组 `[{id/name}]`
fn parse_model_names(body: &Value) -> Vec<String> {
    let pick = |item: &Value| -> Option<String> {
        item.get("id")
            .or_else(|| item.get("name"))
            .or_else(|| item.get("model"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            // 纯字符串数组（["gpt-4", ...]）也支持
            .or_else(|| item.as_str().map(|s| s.to_string()))
    };
    let arr = body
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| body.get("models").and_then(|d| d.as_array()))
        .or_else(|| body.as_array());
    match arr {
        Some(items) => items.iter().filter_map(pick).collect(),
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 多候选端点失败时，报的必须是**最有信息量**的那条，而不是最后一条。
    ///
    /// ## 真机序列（2026-08-22 agentrouter.org，用真实 token 实打取证）
    ///
    /// 1. `/v1/models` → `401 {"type":"unauthorized_client_error",
    ///    "message":"unauthorized client detected"}`  ← **真因：客户端准入**
    /// 2. `/models`    → `200` 但返回站点首页 HTML
    ///
    /// 旧实现报最后一条，界面上写「返回的是网页，请确认 Base URL 通常以 /v1 结尾」——
    /// 而 Base URL 本来就是对的，用户照着改只会越改越错（真机反馈就卡在这里）。
    ///
    /// 四个方向一起钉：
    /// 1. 401 必须压过随后的 HTML/非 JSON（最低级）；
    /// 2. 401 必须压过 404（后者只说明「这条路径不存在」，最没信息量）；
    /// 3. 同级别保留**先出现**的那条（候选顺序本身就是按「最可能是对的」排的）；
    /// 4. 首条无条件采纳。
    #[test]
    fn most_informative_candidate_error_wins_not_the_last_one() {
        let unauthorized = "HTTP 401 Unauthorized @ https://x/v1/models（密钥无效或无权限）：\
                            unauthorized client detected";
        let not_found = "HTTP 404 Not Found @ https://x/models（该路径不存在）";
        let html = "https://x/models 返回的是网页而不是 JSON";

        // ④ 首条无条件采纳
        let e = more_informative_models_error(None, unauthorized, 401);
        assert_eq!(e.as_deref(), Some(unauthorized));

        // ① 401 压过非 JSON（status 0 = 非 HTTP 层失败）
        let e = more_informative_models_error(e, html, 0);
        assert_eq!(
            e.as_deref(),
            Some(unauthorized),
            "拿到 HTML 只说明那个地址不是接口；401 才说清了「为什么被拒」"
        );

        // ② 401 压过 404
        let e = more_informative_models_error(e, not_found, 404);
        assert_eq!(e.as_deref(), Some(unauthorized));

        // ③ 同级别保留先出现的
        let first_500 = "HTTP 500 Internal Server Error @ https://x/v1/models";
        let second_500 = "HTTP 502 Bad Gateway @ https://x/models";
        let e = more_informative_models_error(None, first_500, 500);
        let e = more_informative_models_error(e, second_500, 502);
        assert_eq!(e.as_deref(), Some(first_500), "同级别时先出现的候选优先");

        // 反向：404 打头时，随后的 401 必须**顶替**它
        let e = more_informative_models_error(None, not_found, 404);
        let e = more_informative_models_error(e, unauthorized, 401);
        assert_eq!(
            e.as_deref(),
            Some(unauthorized),
            "404 只说明路径不存在，401 带着上游原话，必须顶替"
        );
    }

    /// 真机反例（2026-08-02）：中转商把请求挡在 HTML 页上，用户只看到
    /// `expected value at line 1 column 1`，完全不知道该改什么。
    /// 提示必须说清「拿到的是网页」并指向 Base URL。
    #[test]
    fn non_json_models_response_says_it_got_a_webpage_not_serde_jargon() {
        let url = "https://nimabo.cn/v1/models";
        for html in [
            "<!DOCTYPE html><html><head><title>登录</title></head></html>",
            "<html><body>Just a moment...</body></html>",
            "  \n<!doctype HTML>\n<html>",
        ] {
            let msg = non_json_models_hint(url, html);
            assert!(msg.contains("网页"), "应说明拿到的是网页，实际：{msg}");
            assert!(msg.contains("Base URL"), "应指向 Base URL，实际：{msg}");
            assert!(msg.contains(url), "应带上具体端点，实际：{msg}");
            // 反例护栏：不得再把 serde 的行列术语抛给用户
            assert!(
                !msg.contains("expected value") && !msg.contains("column"),
                "不该出现 serde 术语，实际：{msg}"
            );
        }
    }

    /// 非 HTML 的垃圾响应：报出开头片段，让用户自己判断拿到了什么；
    /// 空响应单独说，避免出现「开头是「」」这种空洞提示。
    #[test]
    fn non_json_models_response_quotes_head_and_handles_empty() {
        let url = "https://x.test/v1/models";

        let msg = non_json_models_hint(url, "upstream connect error or disconnect");
        assert!(msg.contains("不是合法 JSON"), "实际：{msg}");
        assert!(msg.contains("upstream connect error"), "应引用开头片段，实际：{msg}");

        for empty in ["", "   \n\t  "] {
            let msg = non_json_models_hint(url, empty);
            assert!(msg.contains("空响应"), "空响应应单独措辞，实际：{msg}");
            assert!(!msg.contains("开头是「」"), "不该出现空洞的开头引用，实际：{msg}");
        }
    }

    /// 过长响应体不得整段塞进错误信息（截到 80 字符）。
    #[test]
    fn non_json_models_hint_truncates_long_bodies() {
        let msg = non_json_models_hint("https://x.test/v1/models", &"A".repeat(5000));
        assert!(msg.len() < 400, "错误信息不该被响应体撑爆，长度：{}", msg.len());
    }
}
