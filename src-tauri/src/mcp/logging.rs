use crate::aggregate::{cap_text, McpAggregateResult};
use crate::error::AppResult;
use crate::model::{CategoryType, Protocol, RequestTrace};
use crate::store::Store;

pub(super) fn record_outcome(
    store: &Store,
    category: CategoryType,
    prompt: &str,
    cwd: Option<&str>,
    outcome: &AppResult<McpAggregateResult>,
    elapsed: u64,
) {
    let enabled = store.get_settings().aggregate_trace_enabled;
    let detail = match outcome {
        Ok(result) => format!(
            "synaroute_ai · 参与者 成功{}/发起{}(失败{}) · {}个文件 · {elapsed}ms",
            result.member_labels.len(),
            result.members_attempted,
            result.members_failed,
            result.file_count
        ),
        Err(error) if enabled => format!(
            "synaroute_ai 失败: {} · {elapsed}ms",
            cap_text(&error.to_string())
        ),
        Err(_) => format!("synaroute_ai 失败 · {elapsed}ms（详情记录已关闭）"),
    };
    let trace = enabled.then(|| {
        let (url, members, files, response) = match outcome {
            Ok(result) => (
                result.work_dir.as_deref().unwrap_or_default(),
                cap_text(&result.member_labels.join(" · ")),
                format!("{}个文件", result.file_count),
                cap_text(&result.analysis),
            ),
            Err(error) => (
                cwd.unwrap_or_default(),
                String::new(),
                String::new(),
                cap_text(&error.to_string()),
            ),
        };
        RequestTrace {
            request_id: String::new(),
            key_name: "synaroute_ai".into(),
            vendor: "mcp".into(),
            protocol: Protocol::Anthropic,
            url: cap_text(url),
            requested_model: members,
            real_model: files,
            request_body: cap_text(prompt),
            response_body: response,
            status: outcome.is_ok().then_some(200),
            latency_ms: elapsed,
            ok: outcome.is_ok(),
            was_truncated: None,
        }
    });
    store.append_event_trace(category, "mcp", None, &detail, trace);
}
