//! 上游 URL 拼接：版本段识别、兼容子路径剥离、模型列表候选端点。
//!
//! 三者必须同住：`join_endpoint` 与 `model_endpoints` 都依赖 `ends_with_version_segment`，
//! 而后者的判据（末段是否形如 v1/v1beta）是这一簇的共同前提。

/// 已知的「协议兼容子路径」后缀（借鉴 cc-switch `model_fetch.rs::KNOWN_COMPAT_SUFFIXES`）。
/// 这些不是版本段，而是厂商把 Anthropic/Coding 协议挂在子路径上的兼容前缀
/// （如 DeepSeek `https://api.deepseek.com/anthropic`）。命中时模型列表通常在 host 根、
/// 而非该子路径下，故需剥离后缀回根再探。按长度降序，最长优先匹配。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 若 base_url 以某个已知兼容子路径结尾，返回剥离后缀后的剩余部分。
pub(super) fn strip_compat_suffix(base: &str) -> Option<&str> {
    for suffix in KNOWN_COMPAT_SUFFIXES {
        if let Some(root) = base.strip_suffix(suffix) {
            return Some(root);
        }
    }
    None
}

/// 判断 base_url 的最后一段是否是 OpenAI 风格版本段 `v{N}`（v1/v4/v1beta/v2alpha）。
/// 版本段已在路径里 → 模型/资源端点直接接 `/models`、`/messages`，不再补 `/v1`。
/// 注意：`/anthropic`、`/coding` 等**不是**版本段（不以 v+数字开头）。
pub(super) fn ends_with_version_segment(base: &str) -> bool {
    let last = base.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    match last.strip_prefix('v').or_else(|| last.strip_prefix('V')) {
        // v 后至少一位数字，其余可为字母（兼容 v1beta / v2alpha），整体为字母数字
        Some(rest) => {
            rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                && rest.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// 生成模型列表的候选端点（按优先级）。借鉴 cc-switch `build_models_url_candidates`：
/// - base 已以版本段结尾（/v1、/v4…）：`{base}/models`（非 /v1 时再兜底 `{base}/v1/models`）
/// - 否则（裸 host 或兼容子路径）：`{base}/v1/models`、`{base}/models`
/// - 若 base 命中兼容子路径（/anthropic 等）：追加剥离后的 `{root}/v1/models`、`{root}/models`
///   （DeepSeek 等的 /models 在 host 根，不在 /anthropic 子路径下）
///
/// 结果去重且保持顺序。
pub(super) fn model_endpoints(base: &str) -> Vec<String> {
    let base = base.trim_end_matches('/');
    let mut c: Vec<String> = Vec::new();
    if ends_with_version_segment(base) {
        c.push(format!("{base}/models"));
        if !base.ends_with("/v1") {
            c.push(format!("{base}/v1/models"));
        }
    } else {
        c.push(format!("{base}/v1/models"));
        c.push(format!("{base}/models"));
    }
    if let Some(root) = strip_compat_suffix(base) {
        let root = root.trim_end_matches('/');
        if root.contains("://") {
            c.push(format!("{root}/v1/models"));
            c.push(format!("{root}/models"));
        }
    }
    // 线性去重（候选很少）
    let mut out: Vec<String> = Vec::with_capacity(c.len());
    for url in c {
        if !out.iter().any(|u| u == &url) {
            out.push(url);
        }
    }
    out
}

/// 把「默认带 /v1」的资源路径接到 base_url 上，兼容 base 是否已含版本段（FR-004 修复）。
///
/// 判据是「最后一段是不是版本段 v{N}」（借鉴 cc-switch），而非「有没有路径」——
/// 后者会把 DeepSeek 的兼容前缀 `/anthropic` 误当版本、把 `/v1` 吞掉拼成错误 URL。
/// - base 最后一段是版本段（/v1、/v4、/v1beta）：只接资源名（去掉 path 的 `/v1` 前缀）
/// - 否则（裸 host 或兼容子路径 /anthropic）：原样接 path（补默认 `/v1`）
///
/// 例：
/// - `https://api.anthropic.com` + `/v1/messages` → `.../v1/messages`
/// - `https://api.openai.com/v1` + `/v1/chat/completions` → `.../v1/chat/completions`
/// - `https://open.bigmodel.cn/api/paas/v4` + `/v1/chat/completions` → `.../v4/chat/completions`
/// - `https://api.deepseek.com/anthropic` + `/v1/messages` → `.../anthropic/v1/messages`（关键修复）
pub fn join_endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if ends_with_version_segment(base) {
        let resource = path.strip_prefix("/v1").unwrap_or(path);
        format!("{base}{resource}")
    } else {
        format!("{base}{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_endpoint_handles_version_segments() {
        // 裸 host：补默认 /v1
        assert_eq!(
            join_endpoint("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_endpoint("https://api.deepseek.com", "/v1/chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        // base 已含 /v1：不重复
        assert_eq!(
            join_endpoint("https://api.openai.com/v1", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // base 含非 /v1 版本段：只接资源名（核心修复——旧实现会拼出 /v4/v1/...）
        assert_eq!(
            join_endpoint("https://open.bigmodel.cn/api/paas/v4", "/v1/chat/completions"),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        assert_eq!(
            join_endpoint("https://generativelanguage.googleapis.com/v1beta", "/v1/messages"),
            "https://generativelanguage.googleapis.com/v1beta/messages"
        );
        // trailing slash 归一
        assert_eq!(
            join_endpoint("https://api.openai.com/v1/", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // DeepSeek 兼容前缀 /anthropic 不是版本段：保留 /v1（关键修复，此前会拼成 .../anthropic/messages）
        assert_eq!(
            join_endpoint("https://api.deepseek.com/anthropic", "/v1/messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn version_segment_detection() {
        assert!(ends_with_version_segment("https://x.com/v1"));
        assert!(ends_with_version_segment("https://x.com/api/paas/v4"));
        assert!(ends_with_version_segment("https://x.com/v1beta")); // v + 数字 + 字母
        assert!(!ends_with_version_segment("https://x.com/anthropic")); // 兼容前缀，非版本
        assert!(!ends_with_version_segment("https://api.deepseek.com")); // 裸 host
        assert!(!ends_with_version_segment("https://x.com/coding"));
    }

    #[test]
    fn model_endpoints_cover_deepseek_anthropic_compat() {
        // 关键场景：DeepSeek Anthropic 兼容前缀。/models 在 host 根，故需剥离 /anthropic 追加根候选。
        let eps = model_endpoints("https://api.deepseek.com/anthropic");
        assert!(eps.contains(&"https://api.deepseek.com/anthropic/v1/models".to_string()));
        assert!(eps.contains(&"https://api.deepseek.com/v1/models".to_string()), "应含剥离后的 host 根候选");
        assert!(eps.contains(&"https://api.deepseek.com/models".to_string()));
    }

    #[test]
    fn model_endpoints_version_and_bare() {
        // 版本段 base：{base}/models 优先，非 /v1 再兜底 {base}/v1/models
        assert_eq!(
            model_endpoints("https://open.bigmodel.cn/api/paas/v4"),
            vec![
                "https://open.bigmodel.cn/api/paas/v4/models".to_string(),
                "https://open.bigmodel.cn/api/paas/v4/v1/models".to_string(),
            ]
        );
        // 裸 host：/v1/models 优先，回退 /models
        assert_eq!(
            model_endpoints("https://api.deepseek.com"),
            vec![
                "https://api.deepseek.com/v1/models".to_string(),
                "https://api.deepseek.com/models".to_string()
            ]
        );
    }
}
