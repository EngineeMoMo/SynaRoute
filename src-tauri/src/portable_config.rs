use super::{ExportPayload, ImportMode};
use crate::error::{AppError, AppResult};
use crate::model::{AppConfig, CategoryType};
use crate::store::routing::config::RoutingConfig;
use std::collections::{HashMap, HashSet};

pub(super) fn has_legacy_tokens(payload: &ExportPayload) -> bool {
    payload.keys.iter().any(|key| {
        key.balance_query
            .as_ref()
            .is_some_and(|query| query.access_token.is_some())
    })
}

pub(super) fn take_legacy_tokens(
    payload: &mut ExportPayload,
    include: bool,
) -> HashMap<String, String> {
    let mut secrets = HashMap::new();
    for key in &mut payload.keys {
        if let Some(query) = key.balance_query.as_mut() {
            if let Some(token) = query.access_token.take() {
                let token = zeroize::Zeroizing::new(token);
                query.access_token_ref = if include && !token.is_empty() {
                    let reference = format!(
                        "{}{}",
                        crate::store::balance::ACCESS_PREFIX,
                        uuid::Uuid::new_v4()
                    );
                    secrets.insert(reference.clone(), token.to_string());
                    Some(reference)
                } else {
                    None
                };
            }
        }
    }
    secrets
}

pub(super) fn secret_references(payload: &ExportPayload) -> HashSet<String> {
    let mut references = HashSet::new();
    for key in &payload.keys {
        if !key.id.starts_with("__") {
            references.insert(key.id.clone());
        }
        if let Some(query) = &key.balance_query {
            if let Some(reference) = &query.api_key_ref {
                if !reference.starts_with("__") {
                    references.insert(reference.clone());
                }
            }
            if let Some(reference) = &query.access_token_ref {
                if reference.starts_with(crate::store::balance::ACCESS_PREFIX) {
                    references.insert(reference.clone());
                }
            }
        }
    }
    if let Some(decisions) = &payload.decision {
        for config in decisions.values() {
            if config.secret_id.starts_with("__decision_") {
                references.insert(config.secret_id.clone());
            }
        }
    }
    if let Some(RoutingConfig::Known(routing)) = &payload.routing {
        for policy in routing.categories.values() {
            if let Some(reference) = &policy.privacy.terms_secret {
                if reference.starts_with(crate::store::routing::config::TERMS_PREFIX) {
                    references.insert(reference.clone());
                }
            }
        }
    }
    references
}

pub(super) fn validate_references(payload: &ExportPayload) -> AppResult<()> {
    let invalid = || AppError::Invalid("备份包含无效的内部密钥引用，已拒绝导入".into());
    for key in &payload.keys {
        if key.id.starts_with("__") {
            return Err(invalid());
        }
        if let Some(query) = &key.balance_query {
            if query
                .api_key_ref
                .as_ref()
                .is_some_and(|reference| reference.starts_with("__"))
                || query.access_token_ref.as_ref().is_some_and(|reference| {
                    !reference.starts_with(crate::store::balance::ACCESS_PREFIX)
                })
            {
                return Err(invalid());
            }
        }
    }
    if payload.decision.as_ref().is_some_and(|decisions| {
        decisions.values().any(|config| {
            !config.secret_id.is_empty() && !config.secret_id.starts_with("__decision_")
        })
    }) {
        return Err(invalid());
    }
    validate_routing(payload.routing.as_ref())
}

fn validate_routing(routing: Option<&RoutingConfig>) -> AppResult<()> {
    if let Some(RoutingConfig::Known(routing)) = routing {
        if routing.version == 1 {
            for policy in routing.categories.values() {
                policy.validate()?;
            }
            for scene in &routing.scenes {
                scene.validate()?;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply(
    config: &mut AppConfig,
    payload: &ExportPayload,
    mode: ImportMode,
) -> AppResult<()> {
    if mode == ImportMode::Replace {
        config.routing = payload.routing.clone().unwrap_or_default();
        config.decision = payload.decision.clone().unwrap_or_default();
        if payload.routing.is_none() {
            config
                .settings
                .active_models
                .retain(|_, model| !crate::store::routing::config::is_auto_id(model));
        }
        return validate_routing(Some(&config.routing));
    }
    if let Some(decisions) = &payload.decision {
        config.decision.extend(decisions.clone());
    }
    if let Some(incoming) = &payload.routing {
        match (&mut config.routing, incoming) {
            (RoutingConfig::Known(local), RoutingConfig::Known(incoming))
                if local.version == 1 && incoming.version == 1 =>
            {
                for (category, policy) in &incoming.categories {
                    let current = local.categories.entry(*category).or_default();
                    current.affinity = policy.affinity;
                    current.privacy = policy.privacy.clone();
                    for group in &policy.groups {
                        if let Some(previous) = current
                            .groups
                            .iter_mut()
                            .find(|previous| previous.id == group.id)
                        {
                            *previous = group.clone();
                        } else {
                            current.groups.push(group.clone());
                        }
                    }
                }
                for scene in &incoming.scenes {
                    if let Some(previous) = local
                        .scenes
                        .iter_mut()
                        .find(|previous| previous.id == scene.id)
                    {
                        *previous = scene.clone();
                    } else {
                        local.scenes.push(scene.clone());
                    }
                }
                local.revision = local.revision.max(incoming.revision).saturating_add(1);
            }
            _ => config.routing = incoming.clone(),
        }
    }
    validate_routing(Some(&config.routing))
}

pub(crate) fn reset_runtime(store: &crate::store::Store) {
    for category in CategoryType::ALL {
        store.routing_runtime.reset(category);
    }
    store.routing_runtime.decisions.lock().clear();
    store.routing_runtime.cooldowns.lock().clear();
    store.routing_runtime.previews.lock().clear();
}
