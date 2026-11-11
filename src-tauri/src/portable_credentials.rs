use super::ExportPayload;
use crate::error::{AppError, AppResult};
use crate::model::{AppConfig, BalanceQuery, ProviderKey};
use crate::store::routing::config::RoutingConfig;
use std::collections::HashSet;

pub(super) fn same_provider(local: &ProviderKey, incoming: &ProviderKey) -> bool {
    local.category_id == incoming.category_id
        && local.protocol == incoming.protocol
        && local.base_url.trim().trim_end_matches('/')
            == incoming.base_url.trim().trim_end_matches('/')
}

fn same_balance_destination(local: &BalanceQuery, incoming: &BalanceQuery) -> bool {
    local.template == incoming.template
        && local.url == incoming.url
        && local.method == incoming.method
        && local.auth == incoming.auth
        && local.base_url_override == incoming.base_url_override
        && local.user_id == incoming.user_id
}

pub(crate) fn validate_reuse(
    local: &AppConfig,
    payload: &ExportPayload,
    existing: &[String],
) -> AppResult<()> {
    let existing: HashSet<&str> = existing.iter().map(String::as_str).collect();
    let require_binding = |reference: &str, same: bool| {
        if existing.contains(reference) && !same {
            Err(AppError::Invalid(format!(
                "导入将改变本机凭证「{reference}」的目标地址或引用，已拒绝导入且未改动配置。请先核对接口，在本机显式更新对应凭证，或移除冲突条目及其旧凭证后再导入。"
            )))
        } else {
            Ok(())
        }
    };
    for incoming in &payload.keys {
        let previous = local.keys.iter().find(|key| key.id == incoming.id);
        require_binding(
            &incoming.id,
            previous.is_some_and(|key| same_provider(key, incoming)),
        )?;
        let query = incoming.balance_query.clone().unwrap_or_default();
        let previous_query = previous.map(|key| key.balance_query.clone().unwrap_or_default());
        let same_destination = previous.is_some_and(|key| same_provider(key, incoming))
            && previous_query
                .as_ref()
                .is_some_and(|old| same_balance_destination(old, &query));
        let api_reference = query
            .api_key_ref
            .as_deref()
            .filter(|reference| !reference.is_empty())
            .unwrap_or(&incoming.id);
        let previous_api_reference = previous_query
            .as_ref()
            .and_then(|old| {
                old.api_key_ref
                    .as_deref()
                    .filter(|reference| !reference.is_empty())
            })
            .or_else(|| previous.map(|key| key.id.as_str()));
        require_binding(
            api_reference,
            same_destination && previous_api_reference == Some(api_reference),
        )?;
        if query.access_token.is_none() {
            if let Some(reference) = &query.access_token_ref {
                require_binding(
                    reference,
                    same_destination
                        && previous_query
                            .as_ref()
                            .and_then(|old| old.access_token_ref.as_ref())
                            == Some(reference),
                )?;
            }
        }
    }
    if let Some(decisions) = &payload.decision {
        for (category, incoming) in decisions {
            if incoming.secret_id.is_empty() {
                continue;
            }
            require_binding(
                &incoming.secret_id,
                local.decision.get(category).is_some_and(|previous| {
                    previous.secret_id == incoming.secret_id
                        && previous.provider == incoming.provider
                        && previous.endpoint.trim().trim_end_matches('/')
                            == incoming.endpoint.trim().trim_end_matches('/')
                }),
            )?;
        }
    }
    if let Some(RoutingConfig::Known(incoming)) = &payload.routing {
        for (category, policy) in &incoming.categories {
            if let Some(reference) = &policy.privacy.terms_secret {
                let same = match &local.routing {
                    RoutingConfig::Known(previous) => {
                        previous
                            .categories
                            .get(category)
                            .and_then(|old| old.privacy.terms_secret.as_ref())
                            == Some(reference)
                    }
                    _ => false,
                };
                require_binding(reference, same)?;
            }
        }
    }
    Ok(())
}
