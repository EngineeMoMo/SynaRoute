use super::{config::{is_auto_id, RouteGroup, Settings}, hash_bytes};
use crate::{model::{advertise::AdvertisedModel, CategoryType, ProviderKey}, store::Store};

pub fn groups(settings: &Settings, category: CategoryType) -> &[RouteGroup] {
    settings.categories.get(&category).map(|p| p.groups.as_slice()).unwrap_or(&[])
}

pub fn advertised_for(keys: &[ProviderKey], groups: &[RouteGroup]) -> Vec<AdvertisedModel> {
    let mut models = crate::proxy::model_pool::advertised_pool(keys);
    for group in groups.iter().filter(|g| g.enabled) {
        let outward = group.model_id();
        if !models.iter().any(|m| m.outward == outward) {
            models.push(AdvertisedModel { outward, label: Some(format!("{} · Auto", group.name)) });
        }
    }
    models
}

pub fn advertised(store: &Store, category: CategoryType) -> Vec<AdvertisedModel> {
    let keys = store.enabled_keys_sorted(category);
    match super::settings(store) {
        Ok(config) => advertised_for(&keys, groups(&config, category)),
        Err(_) => crate::proxy::model_pool::advertised_pool(&keys),
    }
}

pub fn models(store: &Store, category: CategoryType) -> Vec<String> {
    advertised(store, category).into_iter().map(|m| m.outward).collect()
}

pub fn resolve_group<'a>(groups: &'a [RouteGroup], model: &str) -> Option<&'a RouteGroup> {
    groups.iter().find(|g| g.enabled && g.model_id() == model)
}

pub fn fingerprint(keys: &[ProviderKey], active: Option<&String>, revision: u64, model: &str) -> String {
    let material: Vec<_> = keys.iter().map(|k| serde_json::json!([
        k.id, k.enabled, k.priority, k.base_url, k.protocol, k.models, k.mappings,
        k.default_model, k.tier_haiku, k.tier_sonnet, k.tier_opus, k.tier_fable,
        k.allow_named_model_fallback
    ])).collect();
    hash_bytes(&serde_json::to_vec(&(material, active, revision, model)).unwrap_or_default())
}

pub fn common_context(group: &RouteGroup, keys: &[ProviderKey]) -> Option<u32> {
    let mut contexts = Vec::new();
    for model in std::iter::once(&group.default_model).chain(group.rules.iter().map(|r| &r.model)) {
        let owners: Vec<_> = keys.iter().filter(|k| crate::proxy::model_pool::serves_natively(k, model)).collect();
        if owners.is_empty() { return None; }
        for key in owners { contexts.push(key.context_window_for_outward(model)?); }
    }
    contexts.into_iter().min()
}

pub fn decorate_codex(catalog: &mut serde_json::Value, groups: &[RouteGroup], keys: &[ProviderKey]) {
    let Some(entries) = catalog.get_mut("models").and_then(serde_json::Value::as_array_mut) else { return; };
    for entry in entries {
        let Some(slug) = entry["slug"].as_str() else { continue; };
        if !is_auto_id(slug) { continue; }
        let Some(group) = resolve_group(groups, slug) else { continue; };
        let context = common_context(group, keys);
        entry["display_name"] = serde_json::json!(format!("{} · Auto", group.name));
        entry["description"] = serde_json::json!("SynaRoute intent routing; model selection may change between user turns.");
        entry["context_window"] = serde_json::json!(context);
        entry["max_context_window"] = serde_json::json!(context);
        entry["supported_reasoning_levels"] = serde_json::json!([]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelInfo;
    #[test]
    fn groups_do_not_invent_keys_or_override_explicit_models() {
        let group = RouteGroup { id: uuid::Uuid::new_v4().to_string(), name:"Work".into(), enabled:true, default_model:"real".into(), judge:None, rules:vec![] };
        let key = ProviderKey { enabled:true, models:vec![ModelInfo { real_name:"real".into(), source:"manual".into(), fetched_at:None, context_window:Some(64000), max_output_tokens:None }], ..Default::default() };
        let models = advertised_for(std::slice::from_ref(&key), std::slice::from_ref(&group));
        assert_eq!(models[0].outward, "real");
        assert_eq!(models[1].outward, group.model_id());
        assert_eq!(common_context(&group, std::slice::from_ref(&key)), Some(64000));
        let mut unknown = key.clone(); unknown.models[0].context_window = None;
        assert_eq!(common_context(&group, &[key, unknown]), None);
    }
}
