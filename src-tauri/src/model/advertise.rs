//! 对外模型清单的**唯一**产出点：既给「对外名」，也给「菜单里该显示什么」。
//!
//! # 为什么要有 label 这一维
//!
//! 本模块之前只有 `serviceable_models() -> Vec<String>`，于是「真实模型名」这一维在这里
//! 被整个丢掉：有映射时对外清单只剩 `expected_name`，而那是个为了过客户端合规判据而
//! 编出来的名字（`claude-sonnet-5-3`）。三个出口拿到的都只有它 ——
//! 用户在 Claude Code / 桌面端的模型菜单里看到的就是那串编造名，
//! 而他真正想确认的是「这一项到底走的是 glm-5.3 还是 deepseek-reasoner」。
//!
//! # 两端都有官方的「显示名」通道（本机实测，非文档推测）
//!
//! - **桌面端**：`inferenceModels[].labelOverride`。官方文档原文（`app.asar`
//!   v1.40609.1.0 @4877555）：「Display label (`labelOverride`) is for IDs the picker can't
//!   derive a friendly name from (Bedrock ARNs, **gateway routing aliases**). **Display-only;
//!   `name` is still what the app sends**」—— 官方点名的用途就包含我们这种网关路由别名。
//!   消费点 @6270516：`r.labelOverride ?? i?.name ?? r.name`（**优先级最高**）。
//!   它**不参与**硬过滤：`io(provider,id)` 扫的是 `t.id`（即 `name`），
//!   故 label 可以是任意串，`glm-5.3` 放进去完全合法。
//! - **CLI**：`/v1/models` 的 `display_name`。`claude.exe` 的 `TSv()` @294227923：
//!   `.map(o => ({value: o.id, label: o.display_name ?? o.id, description: o.description ?? ""}))`。
//!
//! # 🔴 恒等与直连必须给 `None`，不能给「与 outward 相同的那个字符串」
//!
//! 桌面端在**没有** `labelOverride` 时走 `Vwt(name)`（@6400753），把 `claude-opus-5`
//! 派生成友好的「Claude Opus 5」。写一个等于 outward 的 label 反而把它降级成裸 slug ——
//! 也就是「加了功能、界面变丑」。用户的真实配置里就有 `claude-opus-5 → claude-opus-5`
//! 这种恒等行（cc-switch 导出的档里同样常见），所以这不是边缘情形。
//! 收敛在 [`menu_label`] 一处，四个来源共用。

use super::ProviderKey;

/// 一条「我们对客户端宣称的模型」。
///
/// `outward` 是客户端选中后会**发回来**的名字（进 `inferenceModels[].name` /
/// `/v1/models` 的 `id`），也是 `resolve_model` 的输入；
/// `label` 只影响菜单上的文字，`None` = 交给客户端自己派生友好名。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AdvertisedModel {
    pub outward: String,
    pub label: Option<String>,
}

/// 从 Key 上取某个档位配置的取值器。抽成别名只为过 clippy 的 `type_complexity`。
type TierPick = fn(&ProviderKey) -> Option<&String>;

/// 该档位配了之后追加的 Claude 家族代表名。
///
/// 追加的理由：Claude Code / 桌面端会按任务**自己**发这些家族名，
/// 需要它们既能被 `match_tier` 命中、又在选择器里可见。
///
/// 🔴 **`claude-fable-5` 必须显式列出才可用**：CLI 的第二道 filter
/// （`claude.exe` @294227923：`let i = pY(o.id); return i === null || i === Qwt`）会把
/// 「恰好等于某个官方模型 ID」的条目丢掉以免与内置列表重复，而 `Qwt` = fable5 那组是
/// **特例放行**。也就是说 fable 是官方留给第三方部署的一个必须自己声明的档位。
///
/// 🔴 **fable 排在既有三档之后，尽管它是最强档**：`discoverable_models` 的文档写着
/// 「顺序是契约，不只是观感」（三处消费者只看首个）。把新档插到中间，会在用户配了它的那一刻
/// 改变既有三档的相对顺序 —— 而那是一个没人要求、且不会报错的行为变化。
/// UI 上按强度排（快→中→强→最强）是另一回事，两者不必一致。
///
/// ⚠️ **少一条会编译不过**（数组长度标注为 4，实测注入确认），也就是说「档位表不许悄悄变短」
/// 这条由**编译器**保证，不是由某条测试保证 —— 别以为这里有一道机械判据可以放心改。
const TIER_FAMILY: [(&str, TierPick); 4] = [
    ("claude-opus-4-5", |k| k.tier_opus.as_ref()),
    ("claude-sonnet-4-5", |k| k.tier_sonnet.as_ref()),
    ("claude-haiku-4-5", |k| k.tier_haiku.as_ref()),
    ("claude-fable-5", |k| k.tier_fable.as_ref()),
];

/// 本 Key **配了的**档位真实名，按 [`TIER_FAMILY`] 的顺序（opus → sonnet → haiku → fable）。
///
/// 🔴 **加档位时唯一要改的地方就是那张表。** 此前 `fallback_model_for_empty_request` 与
/// `probe_model` 各写了一份 `[&tier_opus, &tier_sonnet, &tier_haiku]` 硬编码三元组 ——
/// 补 Fable 档时**两处都漏了**，而失效方向都是静默的：
/// - 前者：只配了 fable 档的 Key 在「下游没给模型名」时返回 `None`，
///   于是我们对一条明明有模型信息的 Key 说「它没有任何模型信息」；
/// - 后者：健康探测选不出模型 → 退回轻量 `/models` 探测 → 按那个函数自己的注释，
///   会「被上游 401/403 误判失败而反复熔断，即便真实业务完全正常」。
///
/// 有一条判据钉住「生产段里不许再出现那种硬编码档位数组」。
pub(crate) fn configured_tier_models(key: &ProviderKey) -> impl Iterator<Item = &str> {
    TIER_FAMILY
        .into_iter()
        .filter_map(move |(_, pick)| pick(key).map(|s| s.trim()).filter(|s| !s.is_empty()))
}

/// 把一个候选显示名收敛成真正该写下去的 `label`。
///
/// 空白与「与对外名相同」都收敛成 `None` —— 理由见模块头那条 🔴。
/// 大小写不敏感地比较：`Claude-Opus-5` 与 `claude-opus-5` 指的是同一个东西，
/// 而客户端派生的友好名比我们原样回显那串大小写混杂的字符串好看。
fn menu_label(outward: &str, candidate: Option<&str>) -> Option<String> {
    let c = candidate.map(str::trim).filter(|s| !s.is_empty())?;
    if c.eq_ignore_ascii_case(outward.trim()) {
        return None;
    }
    Some(c.to_string())
}

/// 本 Key 对客户端「可点选」的模型（对外名 + 菜单显示名），有序、按对外名去重。
///
/// 规则（与旧 `serviceable_models` 完全一致，只多了 label 这一维）：
/// 1. **有任意完整映射** → 只暴露映射的 `expected_name`；`models` 真实名仅作上游解析/
///    探测素材，不进发现列表（避免「对外名 + 真实名」双暴露）。
///    label 取 `display_name`，留空则取 `real_name` —— **默认就是真实名**，
///    这正是用户要的「菜单里显示真的模型名字」，零配置生效。
/// 2. **无映射** → 暴露 `models` 的 `real_name`（直连场景）。此时对外名本身就是真名，
///    label 一律 `None`。
/// 3. 不论 1/2，已配的档位追加家族代表名，label = 该档的真实名。
pub(crate) fn advertised_models(key: &ProviderKey) -> Vec<AdvertisedModel> {
    let mut out: Vec<AdvertisedModel> = Vec::new();
    let mut push = |outward: &str, candidate: Option<&str>| {
        let outward = outward.trim();
        if outward.is_empty() || out.iter().any(|a| a.outward == outward) {
            return;
        }
        out.push(AdvertisedModel {
            outward: outward.to_string(),
            label: menu_label(outward, candidate),
        });
    };
    let has_mapping = key
        .mappings
        .iter()
        .any(|mp| !mp.expected_name.trim().is_empty() && !mp.real_name.trim().is_empty());
    if has_mapping {
        for mp in &key.mappings {
            if mp.real_name.trim().is_empty() {
                continue;
            }
            // 显示名留空即回落真实名：老配置（没有 displayName 字段）读进来立刻就显示真名。
            let shown = mp
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(mp.real_name.as_str());
            push(&mp.expected_name, Some(shown));
        }
    } else {
        for m in &key.models {
            push(&m.real_name, None);
        }
    }
    for (family, pick) in TIER_FAMILY {
        if let Some(real) = pick(key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            push(family, Some(real));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelInfo, ModelMapping};

    fn mp(expected: &str, real: &str, display: Option<&str>) -> ModelMapping {
        ModelMapping {
            id: format!("m_{expected}"),
            expected_name: expected.into(),
            real_name: real.into(),
            display_name: display.map(str::to_string),
        }
    }

    fn info(real: &str) -> ModelInfo {
        ModelInfo {
            real_name: real.into(),
            source: "manual".into(),
            fetched_at: None,
            context_window: None,
            max_output_tokens: None,
        }
    }

    fn labels(key: &ProviderKey) -> Vec<(String, Option<String>)> {
        advertised_models(key)
            .into_iter()
            .map(|a| (a.outward, a.label))
            .collect()
    }

    /// 只带映射的 Key（`..Default::default()` 形态 —— clippy 的 `field_reassign_with_default`）。
    fn mapped(mappings: Vec<ModelMapping>) -> ProviderKey {
        ProviderKey { mappings, ..Default::default() }
    }

    /// 只带真实模型列表的 Key（直连场景）。
    fn direct(models: Vec<ModelInfo>) -> ProviderKey {
        ProviderKey { models, ..Default::default() }
    }

    /// 只带档位配置的 Key。参数顺序 = 强度递增（opus / sonnet / haiku / fable）。
    fn tiered(opus: Option<&str>, sonnet: Option<&str>, haiku: Option<&str>, fable: Option<&str>) -> ProviderKey {
        let own = |s: Option<&str>| s.map(str::to_string);
        ProviderKey {
            tier_opus: own(opus),
            tier_sonnet: own(sonnet),
            tier_haiku: own(haiku),
            tier_fable: own(fable),
            ..Default::default()
        }
    }

    /// 本轮的**本体**：菜单显示名默认就是映射的真实模型名，用户零配置即可看到 `glm-5.3`。
    #[test]
    fn a_mapping_advertises_the_real_model_name_as_its_menu_label() {
        let k = mapped(vec![mp("claude-sonnet-5-3", "glm-5.3", None)]);
        assert_eq!(
            labels(&k),
            vec![("claude-sonnet-5-3".to_string(), Some("glm-5.3".to_string()))],
            "对外名仍是 expected_name（客户端发回来的就是它），显示名才是真实模型名"
        );
    }

    /// 用户自己写的显示名优先于真实名 —— 那是这个可选字段存在的全部理由（写品牌名/加备注）。
    #[test]
    fn an_explicit_display_name_wins_over_the_real_name() {
        let k = mapped(vec![mp("claude-sonnet-5-3", "glm-5.3", Some("GLM 5.3（思考）"))]);
        assert_eq!(labels(&k)[0].1.as_deref(), Some("GLM 5.3（思考）"));
        // 只有空白的显示名等于没填 —— 否则会写出一个空 labelOverride，桌面端菜单上是一片空白。
        let blank = mapped(vec![mp("claude-sonnet-5-3", "glm-5.3", Some("   "))]);
        assert_eq!(blank.mappings[0].display_name.as_deref(), Some("   "), "夹具本身要带空白");
        assert_eq!(labels(&blank)[0].1.as_deref(), Some("glm-5.3"), "空白回落真实名");
    }

    /// 🔴 恒等映射与直连都必须给 `None` —— 理由见模块头：桌面端在没有 labelOverride 时会把
    /// `claude-opus-5` 派生成「Claude Opus 5」，写一个等于对外名的 label 反而把它降级成裸 slug。
    #[test]
    fn an_identity_mapping_and_a_direct_key_both_advertise_no_label() {
        let identity = mapped(vec![mp("claude-opus-5", "claude-opus-5", None)]);
        assert_eq!(labels(&identity), vec![("claude-opus-5".to_string(), None)]);

        // 大小写不同也算恒等：原样回显一串大小写混杂的名字，不如让客户端自己派生。
        let cased = mapped(vec![mp("claude-opus-5", "Claude-Opus-5", None)]);
        assert_eq!(labels(&cased)[0].1, None, "大小写不敏感地判恒等");

        // 直连（无任何完整映射）：对外名本身就是真实名，label 是纯噪音。
        let d = direct(vec![info("glm-4.6"), info("deepseek-reasoner")]);
        assert_eq!(
            labels(&d),
            vec![("glm-4.6".to_string(), None), ("deepseek-reasoner".to_string(), None)]
        );
    }

    /// 四档：配了就追加家族代表名，label = 该档的真实模型名。
    ///
    /// 顺序断言是**契约**（`discoverable_models` 的三处消费者只看首个）：
    /// fable 尽管是最强档，也必须排在既有三档之后 —— 插到中间会在用户配了它的那一刻
    /// 改变既有三档的相对顺序，而那是个没人要求、也不会报错的行为变化。
    #[test]
    fn configured_tiers_append_family_names_with_the_tier_model_as_label() {
        let k = tiered(
            Some("deepseek-reasoner"),
            Some("glm-4.6"),
            Some("glm-4.5-air"),
            Some("gpt-5.6-sol"),
        );
        assert_eq!(
            labels(&k),
            vec![
                ("claude-opus-4-5".to_string(), Some("deepseek-reasoner".to_string())),
                ("claude-sonnet-4-5".to_string(), Some("glm-4.6".to_string())),
                ("claude-haiku-4-5".to_string(), Some("glm-4.5-air".to_string())),
                ("claude-fable-5".to_string(), Some("gpt-5.6-sol".to_string())),
            ],
            "fable 排最后（顺序是契约）；每档的 label 是它自己的真实模型名"
        );

        // 只配 fable 时也要出现 —— 它是 CLI 那道 `pY` 过滤的特例放行项，不显式列出就用不上。
        let only_fable = tiered(None, None, None, Some("gpt-5.6-sol"));
        assert_eq!(labels(&only_fable), vec![("claude-fable-5".to_string(), Some("gpt-5.6-sol".to_string()))]);

        // 未配的档一个都不许追加（老配置读进来 tier_fable 是 None）。
        assert!(labels(&ProviderKey::default()).is_empty());
    }

    /// 🔴 显示名**不参与**桌面端对外名体检。
    ///
    /// 官方判据 `ro()` 扫的是 `inferenceModels[].name`，而 `labelOverride` 是 display-only
    /// （模块头有原文）。把 label 也送去体检的表现是：用户一填真实模型名就存不进去 ——
    /// 整个功能当场不可用，而报错会指向「模型名不合规」这个完全错误的方向。
    #[test]
    fn a_vendor_named_label_never_makes_the_key_unsaveable() {
        // 对外名合规、显示名却命中官方厂商黑名单（glm）—— 这正是本功能的典型用法。
        let k = ProviderKey {
            category_id: crate::model::CategoryType::ClaudeDesktop,
            mappings: vec![mp("claude-sonnet-5-3", "glm-5.3", Some("glm-5.3"))],
            ..Default::default()
        };
        let report = crate::model::desktop_model_name_report(&k);
        assert!(report.applicable, "桌面端分类才适用");
        assert!(
            report.issues.is_empty(),
            "显示名不该被体检，实际报了: {:?}",
            report.issues
        );
        // 对照：把不合规的名字放到**对外名**上，体检必须报 —— 证明这条用例不是空跑。
        let bad = ProviderKey {
            category_id: crate::model::CategoryType::ClaudeDesktop,
            mappings: vec![mp("glm-5.3", "glm-5.3", None)],
            ..Default::default()
        };
        assert_eq!(crate::model::desktop_model_name_report(&bad).issues.len(), 1);
    }

    /// 🔴 **只配了 fable 档的 Key 也必须能拿到「本 Key 认得的模型」。**
    ///
    /// 这是补 Fable 档时**真的漏掉**的两处（各自失效方向都静默）：
    /// - `fallback_model_for_empty_request`：下游没给模型名时返回 `None`，
    ///   于是我们对一条明明有模型信息的 Key 说「它没有任何模型信息」，让上游去报错；
    /// - `probe_model`：健康探测选不出模型 → 退回轻量 `/models` 探测 →
    ///   按那个函数自己的注释「被上游 401/403 误判失败而反复熔断」。
    #[test]
    fn a_key_with_only_the_fable_tier_still_yields_a_model_it_knows() {
        let only_fable = tiered(None, None, None, Some("gpt-5.6-sol"));
        assert_eq!(
            configured_tier_models(&only_fable).collect::<Vec<_>>(),
            vec!["gpt-5.6-sol"]
        );
        // 空请求名 → 兜底链最后一环。
        assert_eq!(only_fable.resolve_model_detail("").0, "gpt-5.6-sol");
        // 健康探测选模型。
        assert_eq!(only_fable.probe_model().as_deref(), Some("gpt-5.6-sol"));

        // 顺序与对外清单一致（强→弱→fable），且未配的档不出现。
        let all = tiered(Some("ds-r"), Some("glm-4.6"), Some("air"), Some("sol"));
        assert_eq!(
            configured_tier_models(&all).collect::<Vec<_>>(),
            vec!["ds-r", "glm-4.6", "air", "sol"]
        );
        // 只有空白的档等于没配。
        assert!(configured_tier_models(&tiered(Some("  "), None, None, None)).next().is_none());
    }

    /// 生产段（剥注释、剥测试段）—— 本仓已 3 次栽在「注释里的字面量满足了断言」上，
    /// 故源码级判据一律走 `production_code_only`，不用 `production_slice`。
    fn prod(src: &str) -> String {
        crate::proxy::custom_headers::production_code_only(src)
    }

    /// 🔴 **不许再出现硬编码的档位数组。**
    ///
    /// 补 Fable 时漏掉的那两处，形态都是 `[&self.tier_opus, &self.tier_sonnet, &self.tier_haiku]`
    /// —— 加档位这件事需要同时改三个地方，而只有 [`TIER_FAMILY`] 那张表有编译器护栏
    /// （数组长度标注）。判据把「枚举档位」这个动作收敛到那张表上：
    /// 谁再写一份数组，这里就红。
    #[test]
    fn enumerating_tiers_must_go_through_the_one_table() {
        let model_rs = prod(include_str!("../model.rs"));
        for banned in ["self.tier_opus, &self", "tier_opus, &self.tier_sonnet"] {
            assert!(
                !model_rs.contains(banned),
                "又出现了硬编码的档位数组（`{banned}`）—— 用 advertise::configured_tier_models"
            );
        }
        // 正向：那两处真的在走共享访问器（0 处 = 判据失去目标）。
        assert_eq!(
            model_rs.matches("advertise::configured_tier_models(").count(),
            2,
            "fallback_model_for_empty_request 与 probe_model 各一处"
        );
    }

    /// 🔴 **接线判据（本仓第 14 次盯同一类盲区）**：上面那些用例全是直接调 `advertised_models`，
    /// 把三个出口改回「只拿名字」它们照样全绿 —— 而那正是缺陷本体（功能做了、没人用）。
    ///
    /// 四条各盯一处接线；顺带钉住「只有一份实现」。
    #[test]
    fn every_outlet_must_actually_consume_the_label() {
        // ① `serviceable_models` 必须委托本模块，不许把规则再抄一份回去。
        let model_rs = prod(include_str!("../model.rs"));
        assert_eq!(
            model_rs.matches("advertise::advertised_models(").count(),
            1,
            "model.rs 应恰好一处委托；0 = 规则被抄回去了，>1 = 出现了第二个入口"
        );

        // ② `/v1/models` 与 `/v1/models/{id}` **都**要走带 label 的那份。
        //    只改列表那一半，是本仓「修了 A→B、同一个坑必然也在 B→A」的原样复发。
        //    刻意按**各自的函数体**判，不只数总次数 —— 两处调用挤在同一个函数里同样能凑够数。
        let endpoint_rs = prod(include_str!("../proxy/models_endpoint.rs"));
        for f in ["fn handle_list_models", "fn handle_retrieve_model"] {
            let at = endpoint_rs.find(f).unwrap_or_else(|| panic!("{f} 改名了，请同步本判据"));
            let body = &endpoint_rs[at..(at + 900).min(endpoint_rs.len())];
            assert!(
                body.contains("advertised_pool("),
                "{f} 没走带 label 的那份 —— 这个出口的显示名会静默退回对外名"
            );
        }
        assert_eq!(
            endpoint_rs.matches("anthropic_model_json").count(),
            3,
            "列表与检索共用同一个构造函数（1 定义 + 2 调用）；变多说明有人又抄了一份"
        );

        // ③④ 桌面端档：label 要算出来、也要真写进 JSON。
        let profile_rs = prod(include_str!("../tools/desktop_profile.rs"));
        assert!(
            profile_rs.contains("\"labelOverride\""),
            "build_gateway_profile 必须真的写这个键，否则桌面端菜单永远显示派生名"
        );
        assert!(
            profile_rs.contains("label_override: labels.get("),
            "label 必须从 advertised_pool 查表得到，不能恒 None"
        );
    }
}
