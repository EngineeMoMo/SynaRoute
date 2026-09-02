//! Claude 桌面端 3p gateway 配置档的构造：模型条目的能力断言 + 档 JSON。
//!
//! 从 `tools.rs` 抽出来（那个文件棘轮余量为 0），同时也是个自然的边界：
//! 这里只做「把已知事实翻译成官方 schema」，不碰文件系统。
//!
//! # `labelOverride`：让菜单显示真实模型名
//!
//! 判据（`app.asar` v1.40609.1.0，本机实测，非文档推测）：
//! - schema @4870031：`label: "Display name"`, `description: "Shown in the model picker."`,
//!   类型是裸 string（`.trim().transform(e=>e||void 0).optional()`），**无任何校验**。
//! - 官方文档原文 @4877555：「Display label (`labelOverride`) is for IDs the picker can't
//!   derive a friendly name from (Bedrock ARNs, **gateway routing aliases**).
//!   **Display-only; `name` is still what the app sends**」。
//! - 消费点 @6270516（`Gvt`）：`let a = r.labelOverride ?? i?.name ?? r.name` —— 优先级最高。
//! - 🔴 **它不参与硬过滤**：`io(provider,id)` → gateway 走 `Age(id)` → `ro(id)`，扫的是
//!   `t.id`（即 `name`）。故 label 可以是 `glm-5.3` 这种命中厂商黑名单的串，
//!   而 `name` 仍必须过 [`crate::model::is_desktop_acceptable_model_id`]。
//!   **别把 label 也送去体检** —— 那会让整个功能不可用。
//!
//! # 已知副作用（不是缺陷，但不该悄悄发生）
//!
//! 设了 `labelOverride` 后，桌面端会在系统提示词里追加一句
//! `The administrator of this deployment has labeled this model "<label>".`（@14272035）。
//! 方向上是好事（模型不再误以为自己是 Claude Opus），但它改变了模型可见上下文，
//! 故 Key 编辑器里那个输入框旁边如实写明了这一点。

use super::DESKTOP_GATEWAY_PLACEHOLDER;
use crate::model::ProviderKey;
use serde_json::Value;

/// 桌面端 gateway 档里 `inferenceModels` 的一条：对外名 + 由 Key 数据推导出的能力断言。
///
/// 四个字段各有官方语义（判据：`app.asar` 的 `inferenceModels` schema，
/// v1.24012.9 offset ≈ 7013300 / 消费点 ≈ 7400700，v1.40609.1.0 复核未变）：
/// - `supports1m`：**你对自己部署做的能力断言**，只对确认支持 1M 窗口的模型设置。
///   故此处按该对外名解析到的上游模型 `contextWindow` 判定，**无数据时保守 false**——
///   一律写 true 会让桌面端给出一个上游实际不支持、必然失败的 1M 选项。
/// - `anthropic_family_tier`：桌面端遇到裸别名（`opus`/`sonnet`/…）时钉到本条；不填则裸别名无处可落。
/// - `is_family_default`：同档位多条时选谁。同档位内只给**第一条**置 true（官方对多个 true
///   会告警并取首个，我们不制造这种告警）。
/// - `label_override`：**只影响菜单显示**，见模块头。`None` = 交给桌面端自己派生友好名。
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopModelEntry {
    pub name: String,
    pub supports1m: bool,
    pub anthropic_family_tier: Option<&'static str>,
    pub is_family_default: bool,
    pub label_override: Option<String>,
}

/// 把「对外名列表 + 各分类启用 Key」组装成 gateway 档需要的模型条目。
///
/// `keys` 为该分类按优先级排序的启用 Key（与 `discoverable_models` 同源）。
///
/// **窗口只认主 Key**（`keys.first()`，即路由实际优先落点），不逐 Key 找第一个有数据的。
/// 逐 Key 找的问题：主 Key 把 `claude-opus-4-8` 映射到一个没记 `context_window` 的模型
/// （`fetch_models` 拉来的模型一律 `context_window: None`，很常见），备用 Key 恰好记了 1M，
/// 于是写出 `supports1m: true` —— 而请求实际落在主 Key 的 200k 上，桌面端给出一个必然被截断
/// 的选项。查不到就保守写 false：少一个 1M 选项只是少个可选项，多一个假的会让请求直接失败。
///
/// 🔴 **`models` 仍是唯一的顺序来源**：label 只从 `advertised_pool(keys)` 建一张查表来取。
/// 反过来（直接遍历 `advertised_pool`）会引入第二个「档里该有哪些模型」的事实来源，
/// 而调用方给的 `models` 是 `models_for_apply` 的结果、可能已被别处过滤过。
pub fn build_desktop_model_entries(
    models: &[String],
    keys: &[ProviderKey],
) -> Vec<DesktopModelEntry> {
    let labels: std::collections::HashMap<String, String> =
        crate::proxy::model_pool::advertised_pool(keys)
            .into_iter()
            .filter_map(|a| a.label.map(|l| (a.outward, l)))
            .collect();
    let mut tier_seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let primary = keys.first();
    models
        .iter()
        .map(|name| {
            let ctx = primary.and_then(|k| k.context_window_for_outward(name));
            let tier = crate::model::desktop_family_tier_of(name);
            // 同档位只让第一条当默认（官方对多个 isFamilyDefault 会告警并取首个）。
            let is_default = tier.is_some_and(|t| tier_seen.insert(t));
            DesktopModelEntry {
                name: name.clone(),
                supports1m: ctx.is_some_and(|c| c >= crate::model::ONE_MILLION_CONTEXT),
                anthropic_family_tier: tier,
                is_family_default: is_default,
                label_override: labels.get(name.trim()).cloned(),
            }
        })
        .collect()
}

/// 构造 gateway 配置档 JSON（对齐 cc-switch `build_gateway_profile`）。
///
/// **合并写**：在 `existing`（档内既有内容）之上覆盖本函数负责的 7 个键，其余键原样保留——
/// 用户若在桌面端 Setup 面板里给本档加过字段，不会因改端口/重新接入而被静默抹掉。
/// `existing` 非对象（空档/损坏）时按空对象起算。
///
/// `inferenceGatewayApiKey` 用占位（代理剥入站鉴权头、按路由 Key 注入真实密钥）；
/// `inferenceGatewayBaseUrl` 指向本地代理源（桌面端按 Anthropic 风格发 /v1/messages，代理已识别）。
///
/// `disableDeploymentModeChooser` 恒 `true`：**对齐 cc-switch 实测样本**（用户拍板保持一致）。
/// 注意它并非 3p 生效的必需条件——判据 `pd(e) = hasInference(e) && (disableClaudeAiSignIn(e)
/// || persistedMode() !== "1p")`（`app.asar` offset ≈ 7100100）是「或」关系，而我们本就会写
/// `deploymentMode=3p`。代价是接入后桌面端里看不到官方登录入口，须从 SynaRoute 点还原才能回官方。
///
/// `entries` 恒非空（调用方已挡空列表）；每条的能力断言见 [`DesktopModelEntry`]。
pub fn build_gateway_profile(
    existing: Value,
    endpoint: &str,
    entries: &[DesktopModelEntry],
) -> Value {
    let mut obj = match existing {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "coworkEgressAllowedHosts".into(),
        Value::Array(vec![Value::String("*".into())]),
    );
    obj.insert("disableDeploymentModeChooser".into(), Value::Bool(true));
    obj.insert(
        "inferenceGatewayApiKey".into(),
        Value::String(DESKTOP_GATEWAY_PLACEHOLDER.into()),
    );
    obj.insert(
        "inferenceGatewayAuthScheme".into(),
        Value::String("bearer".into()),
    );
    obj.insert(
        "inferenceGatewayBaseUrl".into(),
        Value::String(endpoint.to_string()),
    );
    obj.insert("inferenceProvider".into(), Value::String("gateway".into()));
    // 恒写 inferenceModels（调用方已挡空列表）：合并写下若沿用旧值会与当前可服务集脱节。
    let arr: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut o = serde_json::Map::new();
            o.insert("name".into(), Value::String(e.name.clone()));
            // supports1m 只在确有依据时写：官方语义是能力断言，无依据即不断言。
            if e.supports1m {
                o.insert("supports1m".into(), Value::Bool(true));
            }
            // labelOverride 只在有值时写：无值的语义是「让桌面端自己派生友好名」，
            // 而写一个等于 name 的值反而会把 `Claude Opus 5` 降级成裸 slug（见 advertise 模块）。
            if let Some(label) = &e.label_override {
                o.insert("labelOverride".into(), Value::String(label.clone()));
            }
            if let Some(tier) = e.anthropic_family_tier {
                o.insert("anthropicFamilyTier".into(), Value::String(tier.into()));
                // isFamilyDefault 只在有 tier 时才有意义（官方对「无 tier 却设该标记」会告警并忽略）。
                if e.is_family_default {
                    o.insert("isFamilyDefault".into(), Value::Bool(true));
                }
            }
            Value::Object(o)
        })
        .collect();
    obj.insert("inferenceModels".into(), Value::Array(arr));
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CategoryType, ModelMapping};

    fn desktop_key(expected: &str, real: &str) -> ProviderKey {
        ProviderKey {
            id: "k".into(),
            category_id: CategoryType::ClaudeDesktop,
            enabled: true,
            mappings: vec![ModelMapping {
                id: "m".into(),
                expected_name: expected.into(),
                real_name: real.into(),
                display_name: None,
            }],
            ..Default::default()
        }
    }

    /// 端到端（本模块这一层）：映射的真实模型名要一路走到档 JSON 的 `labelOverride`，
    /// 而 `name` 仍是那个能过官方 `ro()` 的对外名。
    ///
    /// 这两条**必须同时**成立：只改 name 会让桌面端把该条目整个过滤掉
    /// （→ 模型选择器为空 + `ModelsNotDiscoveredError`）；只改 label 则功能没生效。
    #[test]
    fn the_real_model_name_reaches_label_override_while_name_stays_compliant() {
        let key = desktop_key("claude-sonnet-5-3", "glm-5.3");
        let entries = build_desktop_model_entries(
            &["claude-sonnet-5-3".to_string()],
            std::slice::from_ref(&key),
        );
        assert_eq!(entries[0].label_override.as_deref(), Some("glm-5.3"));

        let p = build_gateway_profile(Value::Null, "http://127.0.0.1:1", &entries);
        let m = &p["inferenceModels"][0];
        assert_eq!(m["name"], "claude-sonnet-5-3", "发送用的名字不能变");
        assert_eq!(m["labelOverride"], "glm-5.3", "菜单显示真实模型名");
        assert!(
            crate::model::is_desktop_acceptable_model_id(m["name"].as_str().unwrap()),
            "name 必须仍能过官方判据，否则整条被硬过滤掉"
        );
    }

    /// 恒等映射：`labelOverride` 这个键**整个不写**，不是写一个空串或写等于 name 的值。
    ///
    /// 理由见 `model::advertise` 模块头：没有该键时桌面端走 `Vwt(name)` 派生出
    /// 「Claude Opus 5」，写一个等于 name 的值反而把菜单降级成裸 slug。
    #[test]
    fn an_identity_mapping_writes_no_label_override_key_at_all() {
        let key = desktop_key("claude-opus-5", "claude-opus-5");
        let entries =
            build_desktop_model_entries(&["claude-opus-5".to_string()], std::slice::from_ref(&key));
        assert_eq!(entries[0].label_override, None);
        let p = build_gateway_profile(Value::Null, "http://127.0.0.1:1", &entries);
        assert!(
            p["inferenceModels"][0].get("labelOverride").is_none(),
            "不该出现这个键: {}",
            p["inferenceModels"][0]
        );
    }
}
