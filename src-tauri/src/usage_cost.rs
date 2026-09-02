//! 用量页那张表的行构造：把 `(分类, Key)` 的 token 累计换成**带成本估算与「为什么算不出」的行**。
//!
//! 从 lib.rs 抽出来的（那边冻结在 2209、余量为 0），但这一段本来也该独立：
//! lib.rs 的职责是 IPC 边界，而这里有真正的判定逻辑 —— 尤其是「算不出金额」的**四种成因**，
//! 它们此前被压成同一个 `None`，于是界面只能给一句放之四海的提示，而那句话对三种成因都是假话。
//!
//! # 「—」的四种成因，为什么必须分开
//!
//! 用户看到「—」之后唯一的动作是照提示去修。旧文案是「这条 Key 没有可识别的模型名。
//! 在 Key 里设置默认兜底模型或拉取模型列表后即可估算」—— 它只对第四种成立：
//!
//! | 成因 | 用户能做的事 | 旧文案把他送去做什么 |
//! |---|---|---|
//! | [`UnpricedReason::Aggregate`]（大脑聚合，`keyId` 为空串） | 无（没有 Key 可设） | 去一个不存在的 Key 里设兜底模型 |
//! | [`UnpricedReason::KeyDeleted`] | 无（Key 已删且**连墓碑也没有**） | 同上 |
//! | [`UnpricedReason::ModelNotInTable`] | 填计费倍率 / 反馈补表 | 设兜底模型（设了照旧算不出） |
//! | [`UnpricedReason::NoModelName`] | **设兜底模型或拉模型列表** | ✅ 只有这一种是对的 |
//!
//! 把它们并成一个 `None` 的代价不是「提示不精确」，而是**让用户做无效操作后仍看不到任何新信息**。
//!
//! ⚠️ **`KeyDeleted` 现在是罕见分支**：[`usage_keys`] 会在 Key 还活着时把算钱要的事实
//! （名字 / 代表模型 / 倍率）记下来，删除之后那条记录就是墓碑，金额照旧算得出。
//! 只有「本功能上线之前就已删除」的历史行才会落到这一支 —— 那些行的模型名与倍率
//! 在删除那一刻就不在任何地方了，**不编造一个模型名去凑金额**（见 `usage_keys` 模块头）。

use crate::model::CategoryType;
use crate::pricing::PricingSource;
use crate::store::Store;

#[path = "usage_keys.rs"] pub(crate) mod usage_keys; // 已删 Key 的金额还原事实；来由见该文件模块注释

/// 为什么这一行算不出金额。`None` = 算出来了。
///
/// 与 [`PricingSource`] 是两个不同的维度，别合并：`PricingSource` 回答「这个数字有多准」，
/// 本枚举回答「为什么没有数字」。合并成一个枚举会让「精确命中」与「Key 已删」挤在同一个
/// 判别式里，而界面对它们的处置完全不同（一个标 ≈、一个要解释）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum UnpricedReason {
    /// 大脑聚合的用量：`keyId` 为空串，压根没有 Key 可以查倍率/代表模型。
    ///
    /// 这是**历史遗留行**才会出现的形态 —— `aggregate.rs` 现在会把用量记到真实参与者
    /// 的 Key 上（见那边 `append_usage_event` 的判据）。旧版本攒下的空 keyId 行会停止增长
    /// 但永远留在表上（用量是纯累加值，不该改写历史），故这一支必须一直在。
    Aggregate,
    /// 这一行指向的 Key 已被删除，**且连墓碑也没有**（[`usage_keys`] 上线之前删的）。
    /// 历史用量保留，但取不到倍率与代表模型 —— 用户此时确实什么也做不了。
    KeyDeleted,
    /// Key 在，但既没配 `default_model`、`models` 也是空的 → 没有代表模型可查价。
    NoModelName,
    /// 有模型名，但单价表里没有它。界面要**点出这个名字**，否则用户无从反馈。
    ModelNotInTable { model: String },
}

/// 带成本估算的用量行（用量页表格用）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageCostRow {
    pub(crate) category_id: CategoryType,
    pub(crate) key_id: String,
    /// Key 的可读名（keyId 是 uuid，用户认不出）。Key 已删除时**从墓碑还原**，
    /// 只有连墓碑也没有时才是 None。
    pub(crate) key_name: Option<String>,
    /// 这一行的 Key 已经不在配置里了。
    ///
    /// 🔴 **必须单独给一位**：`key_name` 现在对已删 Key 也有值（从墓碑还原），
    /// 于是界面光看名字**分辨不出这条 Key 还在不在** —— 用户会以为它还能用。
    /// 加墓碑之前不需要这一位，因为那时名字是空的、表格显示 uuid，异常一眼可见。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) key_deleted: bool,
    pub(crate) usage: crate::model::TokenUsage,
    /// 估算成本（纳美元）。`None` = 没有可用单价，界面显示「—」而不是 0。
    pub(crate) cost_nano: Option<u64>,
    /// 单价来源，界面据此标注精度（exact / family / unknown）。
    pub(crate) pricing_source: PricingSource,
    /// 实际生效的倍率（回显用户填的值，空则为 "1.0"）。
    pub(crate) multiplier: String,
    /// 算不出时的成因。`Some(..)` ⇔ `cost_nano.is_none()`，由
    /// `reason_is_present_exactly_when_cost_is_absent` 钉住这条等价关系。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unpriced_reason: Option<UnpricedReason>,
    /// 用来估算的代表模型名（有的话）。界面显示「按 <model> 估算」——
    /// 不显示它的话，用户无从判断这个金额是拿哪个档位算出来的，而同一条 Key
    /// 跑多个档位模型时偏差恰恰来自这里。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priced_by_model: Option<String>,
}

/// 这条 Key 的**代表模型**：优先用户配的兜底模型，否则模型列表首个。
///
/// 抽成函数是因为它现在有两个调用方（本行的实时计算 + 写进 `usage_keys` 墓碑），
/// 两处各写一份的话，墓碑记下的模型与活着时用的模型迟早不是同一个 ——
/// 而那种漂移的表现是「删 Key 前后金额不一样」，静默且没人会想到是这里。
fn repr_model_of(k: &crate::model::ProviderKey) -> Option<String> {
    k.default_model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| k.models.first().map(|m| m.real_name.clone()))
}

/// 按「分类 × Key」聚合的用量 **+ 成本估算**。
///
/// 成本按 Key 而非按模型估算：用量累加器的键不含模型名（见 `pricing::estimate_cost` 的文档）。
/// 代表模型取该 Key 的 `default_model`，没配则取模型列表首个。
///
/// **Key 已删的行走墓碑**（[`usage_keys`]）：算钱要的两样东西只存在于 `ProviderKey` 里，
/// 而用量是纯累加值、删 Key 之后仍保留 —— 不留墓碑的话那些行的金额永远是「—」。
pub(crate) fn rows(store: &Store) -> Vec<UsageCostRow> {
    // 顺带把「当前还活着的 Key 的算钱事实」记下来，并拿回合并后的全表。
    // 挂在读路径上的理由 + 只在内容变化时才写盘，见 usage_keys 模块头。
    let today = usage_keys::today_ms(chrono::Utc::now().timestamp_millis());
    let live: std::collections::BTreeMap<String, usage_keys::KeyFacts> = CategoryType::ALL
        .iter()
        .flat_map(|c| store.list_keys(*c))
        .map(|k| {
            (
                k.id.clone(),
                usage_keys::KeyFacts {
                    name: k.name.clone(),
                    repr_model: repr_model_of(&k),
                    multiplier: k.cost_multiplier.clone(),
                    seen_day_ms: today,
                },
            )
        })
        .collect();
    let facts = usage_keys::sync(
        std::path::Path::new(&store.config_path_display()),
        live,
    );
    store
        .token_usage_by_key()
        .into_iter()
        .map(|r| {
            let key = store.get_key(&r.key_id);
            // Key 已删 → 回落到墓碑（它活着时最后一次被记下的事实）。
            let tomb = if key.is_none() { facts.get(&r.key_id) } else { None };
            let hint = key
                .as_ref()
                .and_then(repr_model_of)
                .or_else(|| tomb.and_then(|t| t.repr_model.clone()));
            let mult = key
                .as_ref()
                .and_then(|k| k.cost_multiplier.clone())
                .or_else(|| tomb.and_then(|t| t.multiplier.clone()))
                .unwrap_or_else(|| "1.0".into());
            let (cost_nano, pricing_source) =
                crate::pricing::estimate_cost(&r.usage, hint.as_deref(), Some(&mult));

            // 成因判定的顺序 = 从「最外层的缺失」到「最内层的缺失」，
            // 每一层都排除了它下面那层的可能性，所以不会答出误导性的成因。
            let unpriced_reason = if cost_nano.is_some() {
                None
            } else if r.key_id.is_empty() {
                Some(UnpricedReason::Aggregate)
            } else if key.is_none() {
                Some(UnpricedReason::KeyDeleted)
            } else {
                match hint.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Some(UnpricedReason::NoModelName),
                    Some(m) => Some(UnpricedReason::ModelNotInTable { model: m.to_string() }),
                }
            };

            UsageCostRow {
                category_id: r.category_id,
                // 聚合行（key_id 为空串）不算「已删除」——它压根没有 Key。
                // 必须在 `key_id` 被 move 进去之前算。
                key_deleted: key.is_none() && !r.key_id.is_empty(),
                key_id: r.key_id,
                key_name: key
                    .as_ref()
                    .map(|k| k.name.clone())
                    .or_else(|| tomb.map(|t| t.name.clone())),
                usage: r.usage,
                cost_nano,
                pricing_source,
                multiplier: mult,
                unpriced_reason,
                priced_by_model: cost_nano.is_some().then_some(hint).flatten(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CategoryType, TokenUsage};
    use crate::service::tests::{key, temp_store};

    /// 四种成因必须**各不相同**，且与「有没有 Key」「有没有模型名」的事实一致。
    ///
    /// 故障注入判据：把 `Aggregate` 那一支删掉、退回 `NoModelName` → 本测试必须变红
    /// （那正是旧行为：聚合行被当成「你去 Key 里设个兜底模型」，而它压根没有 Key）。
    #[test]
    fn unpriced_reason_distinguishes_all_four_causes() {
        let (store, dir) = temp_store("unpriced");

        // ① 聚合行：key_id 为空串（旧版本 aggregate.rs 留下的形态）
        store.append_event_full(
            CategoryType::Codex,
            "aggregate",
            None,
            "聚合",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 10, output: 1, ..Default::default() }),
        );
        // ② 指向已删除 Key 的行
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("ghost-key"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 20, output: 2, ..Default::default() }),
        );
        // ③ Key 在、但无任何模型名
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "nomodel".into();
        k.default_model = None;
        k.models = vec![];
        store.upsert_key(k).unwrap();
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("nomodel"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 30, output: 3, ..Default::default() }),
        );
        // ④ Key 在、有模型名，但表里没有它
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "weird".into();
        k.default_model = Some("totally-unknown-llm-9000".into());
        store.upsert_key(k).unwrap();
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("weird"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 40, output: 4, ..Default::default() }),
        );

        let rows = rows(&store);
        let find = |kid: &str| {
            rows.iter()
                .find(|r| r.key_id == kid)
                .unwrap_or_else(|| panic!("找不到 key_id={kid} 的行"))
        };

        assert_eq!(find("").unwrap_or_reason(), UnpricedReason::Aggregate);
        assert_eq!(find("ghost-key").unwrap_or_reason(), UnpricedReason::KeyDeleted);
        assert_eq!(find("nomodel").unwrap_or_reason(), UnpricedReason::NoModelName);
        assert_eq!(
            find("weird").unwrap_or_reason(),
            UnpricedReason::ModelNotInTable { model: "totally-unknown-llm-9000".into() },
            "认不出的模型名必须原样带出来，否则用户无从反馈要补哪一条"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `unpriced_reason.is_some()` 与 `cost_nano.is_none()` 必须**严格等价**。
    ///
    /// 两个字段各自独立赋值，很容易一边改了另一边没改 —— 而漂移的表现是静默的：
    /// 界面既显示了金额、又在横幅里把它计进「算不出的条数」，或者反过来显示「—」却不给成因。
    #[test]
    fn reason_is_present_exactly_when_cost_is_absent() {
        let (store, dir) = temp_store("reason_equiv");
        // 一条能算出来的（claude-sonnet 在表里）
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "good".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        store.upsert_key(k).unwrap();
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("good"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 1000, output: 100, ..Default::default() }),
        );
        // 一条算不出来的
        store.append_event_full(
            CategoryType::Codex,
            "aggregate",
            None,
            "聚合",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 5, output: 1, ..Default::default() }),
        );

        for r in rows(&store) {
            assert_eq!(
                r.cost_nano.is_none(),
                r.unpriced_reason.is_some(),
                "key_id={} 两个字段不等价：cost={:?} reason={:?}",
                r.key_id,
                r.cost_nano,
                r.unpriced_reason
            );
            // 能算出来的行必须回显「按哪个模型算的」，否则用户看不出偏差来自哪。
            assert_eq!(
                r.cost_nano.is_some(),
                r.priced_by_model.is_some(),
                "能算出金额就必须回显代表模型"
            );
        }

        assert_eq!(
            rows(&store).iter().filter(|r| r.cost_nano.is_some()).count(),
            1,
            "claude-sonnet-4-5 必须能算出金额（若为 0，说明单价表回归了）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **用户报的问题**：「key 删除了用量要可看，价格应该是算好的」。
    ///
    /// 修之前：删一条 Key 之后它那些行的金额永远是「—」（算钱要的代表模型与倍率
    /// 只存在于 `ProviderKey` 里，而用量是纯累加值、删了也保留）。
    #[test]
    fn a_deleted_key_still_gets_a_price_from_its_tombstone() {
        let (store, dir) = temp_store("tomb_price");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "doomed".into();
        k.name = "要被删掉的站".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        k.cost_multiplier = Some("2.0".into());
        store.upsert_key(k).unwrap();
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("doomed"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 1000, output: 100, ..Default::default() }),
        );

        // ① Key 还活着：算得出金额，并顺带把事实写进墓碑。
        let before = rows(&store);
        let alive = before.iter().find(|r| r.key_id == "doomed").expect("有这一行");
        let priced = alive.cost_nano.expect("Key 活着时必须能算出金额");
        assert_eq!(alive.multiplier, "2.0");

        // ② 删掉它。
        store.delete_key("doomed").unwrap();
        assert!(store.get_key("doomed").is_none(), "前提：Key 真的删掉了");

        // ③ 金额必须还在，且与删之前**完全一致**（同一套 estimate_cost + 同样的事实）。
        let after = rows(&store);
        let gone = after.iter().find(|r| r.key_id == "doomed").expect("用量行必须保留");
        assert_eq!(
            gone.cost_nano,
            Some(priced),
            "删 Key 之后金额变了或没了 —— 这正是本功能要修的问题"
        );
        assert_eq!(gone.unpriced_reason, None, "算出来了就不该有成因");
        assert_eq!(gone.multiplier, "2.0", "倍率要从墓碑里还原，否则金额会按 1.0 缩水");
        assert_eq!(
            gone.key_name.as_deref(),
            Some("要被删掉的站"),
            "名字也要还原 —— key_id 是 uuid，用户认不出这行是谁"
        );
        assert!(
            gone.key_deleted,
            "名字既然还原了，就必须另给一位说明「这条 Key 已经不在了」——\
             否则界面上它与一条正常在用的 Key 长得一模一样"
        );
        assert!(!alive.key_deleted, "还活着的时候这一位必须是 false");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 墓碑文件**只在内容真变了时才写**：`rows()` 被前端轮询调用，
    /// 无脑写会让一个开着用量页的窗口每秒制造一次写入 ——
    /// 与 `usage_dirty` 那条「空闲的应用一个字节都不写盘」纪律直接冲突。
    #[test]
    fn repeated_reads_do_not_rewrite_the_tombstone_file() {
        let (store, dir) = temp_store("tomb_idle");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "steady".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        store.upsert_key(k).unwrap();
        let path = usage_keys::file_path(std::path::Path::new(&store.config_path_display()));

        let _ = rows(&store);
        let first = std::fs::metadata(&path).expect("第一次读就该建出墓碑文件");
        let stamp = first.modified().ok();
        // 连读若干次，内容没变 → 一个字节都不该再写。
        for _ in 0..5 {
            let _ = rows(&store);
        }
        let again = std::fs::metadata(&path).unwrap();
        assert_eq!(again.len(), first.len(), "文件长度变了说明重写过");
        if let (Some(a), Ok(b)) = (stamp, again.modified()) {
            assert_eq!(a, b, "mtime 变了说明重写过（轮询会把盘写爆）");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 空白 `default_model` 不能被当成有效模型名（那会让查价拿一个空串去比对，
    /// 落到 `ModelNotInTable { model: "" }` 这种对用户毫无意义的成因）。
    #[test]
    fn blank_default_model_falls_through_to_the_model_list() {
        let (store, dir) = temp_store("blank_hint");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "blank".into();
        k.default_model = Some("   ".into());
        k.models = vec![crate::model::ModelInfo {
            real_name: "claude-sonnet-4-5".into(),
            source: "manual".into(),
            fetched_at: None,
            context_window: None,
            max_output_tokens: None,
        }];
        store.upsert_key(k).unwrap();
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("blank"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 1000, output: 0, ..Default::default() }),
        );

        let r = rows(&store).into_iter().find(|r| r.key_id == "blank").unwrap();
        assert!(r.cost_nano.is_some(), "空白兜底模型应回退到 models[0]");
        assert_eq!(r.priced_by_model.as_deref(), Some("claude-sonnet-4-5"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 测试内的小助手：把 `Option<UnpricedReason>` 解出来（断言失败时的信息更有用）。
    trait ReasonExt {
        fn unwrap_or_reason(&self) -> UnpricedReason;
    }
    impl ReasonExt for UsageCostRow {
        fn unwrap_or_reason(&self) -> UnpricedReason {
            self.unpriced_reason
                .clone()
                .unwrap_or_else(|| panic!("key_id={} 本该算不出金额，却算出了 {:?}", self.key_id, self.cost_nano))
        }
    }

    /// `TokenUsage` 的 `Default` 在测试里被大量用到（只填 input/output），这条锁住它是全 0。
    #[test]
    fn token_usage_default_is_all_zero() {
        let u = TokenUsage::default();
        assert_eq!((u.input, u.output, u.cache_read, u.cache_creation), (0, 0, 0, 0));
    }
}
