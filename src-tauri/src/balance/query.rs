use super::{
    failure_explains_root_cause, query_one_endpoint, resolve_endpoint_candidates, BalanceQuery,
    BalanceResult, ProviderKey,
};

pub async fn query_balance(
    key: &ProviderKey,
    config: &BalanceQuery,
    secret: &str,
) -> BalanceResult {
    let mut result = query_balance_inner(key, config, secret).await;
    result.error = result.error.map(|error| redact(&error, config, secret));
    result
}

/// 对单个 Key 执行一次余额查询。
///
/// `secret` 由调用方从密钥库取出（本模块不碰密钥库，保持可测）。
///
/// **可能按序试多个端点**：用户没填地址、域名又认不出时走
/// [`super::FALLBACK_ENDPOINTS`] 探测，命中即停并把命中的模板放进
/// `resolved_url_template` 供调用方写回配置（此后每次只发 1 个请求）。
/// 单端点场景（用户填了 / 域名认得出）行为与从前完全一致。
async fn query_balance_inner(key: &ProviderKey, cfg: &BalanceQuery, secret: &str) -> BalanceResult {
    if !cfg.enabled {
        return BalanceResult::failed("未启用余额查询");
    }

    let base = cfg
        .base_url_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&key.base_url);

    let candidates = resolve_endpoint_candidates(cfg, base);
    let probing = candidates.len() > 1;
    let mut first_err: Option<BalanceResult> = None;
    // 「有信息量」的失败：它解释了**为什么**查不到（目前只有「只给了配额上限」这一种）。
    // 优先它而不是第一条 —— 否则最终报告是第一个候选那句 404，而真正的原因被丢了。
    let mut informative_err: Option<BalanceResult> = None;

    for (url_template, auth_scheme) in &candidates {
        let mut r = query_one_endpoint(key, cfg, secret, base, url_template, auth_scheme).await;
        if r.ok {
            // 只在真探测过时才回写：单端点场景没什么可「记住」的，回写等于把
            // 自动识别的结果固化成用户配置，日后我们改进识别表就再也不生效了。
            if probing {
                r.resolved_url_template = Some((*url_template).to_string());
            }
            return r;
        }
        // 瞬时失败（并发去重哨兵）不代表端点不对，也不该继续打其余端点。
        if r.transient {
            return r;
        }
        if informative_err.is_none() && r.error.as_deref().is_some_and(failure_explains_root_cause)
        {
            informative_err = Some(r.clone());
        }
        if first_err.is_none() {
            first_err = Some(r);
        }
    }

    // 全部候选都失败：优先报**解释了原因**的那条，其次报第一条（最可能对的那个端点）。
    // 只报最后一条会让用户去查 `hard_limit_usd` 那个最不可能的路径。
    let mut out = informative_err
        .or(first_err)
        .unwrap_or_else(|| BalanceResult::failed("没有可用的查询端点"));
    if probing {
        let tried: Vec<&str> = candidates.iter().map(|(u, _)| *u).collect();
        let note = format!("\n（已自动尝试 {} 个常见端点均失败：{}。若站点的余额接口不在其中，请在「查询地址」里手填。）", tried.len(), tried.join("、"));
        out.error = Some(match out.error.take() {
            Some(e) => format!("{e}{note}"),
            None => note.trim_start().to_string(),
        });
    }
    out
}

pub(super) fn redact(text: &str, config: &BalanceQuery, secret: &str) -> String {
    crate::diagnostics::redact_secret(
        &crate::diagnostics::redact_secret(text, secret),
        config.access_token.as_deref().unwrap_or_default(),
    )
}

pub(super) async fn response_text(response: reqwest::Response) -> crate::error::AppResult<String> {
    let bytes = crate::upstream::response_body::read_limited(
        response,
        crate::upstream::response_body::ERROR_LIMIT,
        |_| {},
    )
    .await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
