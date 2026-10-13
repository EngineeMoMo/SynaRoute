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
//! ⚠️ **「已删除」行现在只剩两种来源**：2026-10-12 起「删 Key」会把它的用量**一并删除**
//! （[`purge_key`]，用户定的口径，推翻 09-02 的「删了照样算钱」）。剩下的是本版本之前删掉的 Key，
//! 以及绕过删除按钮离开配置的 Key（导入时整份替换 / 手改 config.json）—— 后者靠 [`usage_keys`]
//! 的墓碑照旧算得出金额；连墓碑也没有的才落到 `KeyDeleted`，**不编造一个模型名去凑金额**，
//! 用户可在用量页上把这类行清掉。
//!
//! # 按模型估价（2026-10-12）
//!
//! 账本的键是 `(分类, Key, 真实模型)`（见 `usage_ledger`），每一格按**自己的**模型查价，一行的
//! 金额 = 各格之和。只有 v3 及更早留下的、没记模型的那一格（`model` 为空串）才按 Key 的代表模型
//! 估 —— 那是历史数据唯一能用的近似，来源标成 `Family`（界面打「≈」）。

use crate::model::{CategoryType, ProviderKey, TokenUsage, TokenUsageByKey};
use crate::pricing::PricingSource;
use crate::store::Store;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[path = "usage_keys.rs"] pub(crate) mod usage_keys; // 已删 Key 的金额还原事实；来由见该文件模块注释

/// 为什么这一行（或其中一部分）算不出金额。`None` = 全部算出来了。
///
/// 一行由若干「模型格」组成（见模块头「按模型估价」）；有几格算不出时取 **token 最多的那一格**的成因
/// —— 横幅按成因分组、一行只进一组，完整的逐格情况在 `breakdown` 里。
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
    /// 这一行的 Key 已不在配置里、**且连墓碑也没有**，而那一格又没记模型（升级前的历史用量）
    /// → 取不到代表模型。用户能做的只有在用量页上把这一行清掉（[`purge_key`]）。
    KeyDeleted,
    /// Key 在，但那一格没记模型（升级前的历史用量），而 Key 既没配 `default_model`、
    /// `models` 也是空的 → 没有代表模型可查价。
    NoModelName,
    /// 有名字可查（真实模型名，或历史格的代表模型），但单价表里没有它。界面要**点出这个名字**。
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
    /// 这一行**是否真的捕获过上游回报的 usage**。
    ///
    /// 🔴 **不能用「四个 token 数是否全 0」代替它**。用户报的问题是「新加的 key 都看不见」：
    /// 行集合此前是 `usage_totals` 的投影，而正常采集对空/全零 usage 一律返回 `None`
    /// （见 `upstream::usage::extract_usage`），所以一条**从未产生过可计量用量**的 Key
    /// 压根没有桶 → 无论刷新多少次、等多久都不出现。现在行集合改成
    /// 「配置里的 Key ∪ 历史累计桶」，这一位区分两种**处置完全不同**的 0：
    ///
    /// - `false`：我们还没收到任何 usage。可能尚未使用、流still在进行、或上游不回 usage。
    ///   金额必须是「—」而**不是 $0**，且不该计进「算不出金额」的告警条 —— 那条是让用户
    ///   去改配置的，而这里没有任何配置需要改。
    /// - `true`：真有过消耗，数字是事实。
    ///
    /// 把两者混起来的代价是**界面撒谎**：一条刚建好、还没跑过一次请求的 Key 显示
    /// 「花费 $0.0000」，用户会据此认为它已经在服务且完全免费。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) has_recorded_usage: bool,
    /// 估算成本（纳美元）= 能算出的那几格之和。`None` = 一格都算不出，界面显示「—」而不是 0。
    pub(crate) cost_nano: Option<u64>,
    /// 单价来源，界面据此标注精度：有任一格是按家族 / 代表模型估的就是 `family`（打「≈」）。
    pub(crate) pricing_source: PricingSource,
    /// 实际生效的倍率（回显用户填的值，空则为 "1.0"）。
    pub(crate) multiplier: String,
    /// 有任一格算不出时的成因。`cost_nano.is_none()` ⟹ 它一定是 `Some`；**反之不成立** ——
    /// 部分格算出、部分没有时两者都有（界面标「部分未计入」）。由
    /// `reason_is_present_exactly_when_some_part_is_unpriced` 钉住。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unpriced_reason: Option<UnpricedReason>,
    /// 算出金额的那几格里**花费最多**的那个查价名。界面显示「主要按 <model> 估算」，
    /// 让用户一眼看出这个金额主要是拿哪个模型的价算的；逐格拆分见 `breakdown`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priced_by_model: Option<String>,
    /// 逐个模型的用量与估价（花费从高到低）。占位行为空。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) breakdown: Vec<UsagePartCost>,
    /// 用户为这条 Key 设的花费预算（美元，累计口径）。None = 未设 / 已删 Key（墓碑不存预算）。
    /// 界面据此画「$已花 / $预算」进度条。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) budget_usd: Option<f64>,
    /// 累计估算花费是否已达/超过预算。仅在设了预算、且算得出金额时可能为 true。
    /// 🔴 **单一事实来源**：前端不自己比大小，直接渲染这一位 —— 两处各算一遍必然漂移
    /// （表头说超了、进度条没红，或反过来）。判定口径见 [`budget_status`]。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) over_budget: bool,
}

/// 一行里的一格：某个模型的用量与估价（「按模型拆分」那张 tooltip 用）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsagePartCost {
    /// 上游真实模型名。空串 = 升级前没记模型的历史用量。
    pub(crate) model: String,
    /// 实际拿去查价的名字：有模型名就是它，历史格是 Key 的代表模型；`None` = 无名可查。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priced_as: Option<String>,
    pub(crate) usage: TokenUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cost_nano: Option<u64>,
    pub(crate) pricing_source: PricingSource,
}

/// 一格的估价。有模型名就按它；没有（v3 历史格）才按 Key 的代表模型 `repr`，且标成 `Family` ——
/// 那只是一个猜测（正是它把一条 Key 的输入低估了 50 倍），界面必须打「≈」。
fn part_cost(r: &TokenUsageByKey, repr: Option<&str>, mult: Option<&str>) -> UsagePartCost {
    let (priced_as, legacy) = match r.model.trim() {
        "" => (repr.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string), true),
        m => (Some(m.to_string()), false),
    };
    let (cost_nano, src) = crate::pricing::estimate_cost(&r.usage, priced_as.as_deref(), mult);
    let pricing_source = if legacy && cost_nano.is_some() { PricingSource::Family } else { src };
    UsagePartCost { model: r.model.clone(), priced_as, usage: r.usage, cost_nano, pricing_source }
}

/// 一条 Key 的逐格估价 + 合计。**用量页与路由降级（预算）共用这一处**：两边各算一遍的话
/// 「用量页标红了、路由却没降级」这类漂移必然出现，且失效是静默的。
struct KeyCost {
    /// 花费从高到低（算不出的按 token 数排在后面）。
    parts: Vec<UsagePartCost>,
    /// 能算出的那几格之和；一格都算不出时为 `None`。
    total: Option<u64>,
}

fn key_cost(rows: &[TokenUsageByKey], repr: Option<&str>, mult: Option<&str>) -> KeyCost {
    let mut parts: Vec<UsagePartCost> = rows.iter().map(|r| part_cost(r, repr, mult)).collect();
    parts.sort_by(|a, b| b.cost_nano.cmp(&a.cost_nano).then(token_sum(&b.usage).cmp(&token_sum(&a.usage))));
    let total = parts.iter().filter_map(|p| p.cost_nano).reduce(u64::saturating_add);
    KeyCost { parts, total }
}

/// 四项 token 之和（输入 + 输出 + 缓存读 + 缓存写），与用量页「合计」列同口径。
/// `TokenUsage::total` 只数输入与输出，别拿它代替。
fn token_sum(u: &TokenUsage) -> u64 {
    u.input.saturating_add(u.output).saturating_add(u.cache_read).saturating_add(u.cache_creation)
}
/// 这条 Key 的**代表模型**：优先用户配的兜底模型，否则模型列表首个。只用于没记模型的历史格。
///
/// 抽成函数是因为它有两个调用方（本行的实时计算 + 写进 `usage_keys` 墓碑），
/// 两处各写一份的话，墓碑记下的模型与活着时用的模型迟早不是同一个 ——
/// 而那种漂移的表现是「删 Key 前后金额不一样」，静默且没人会想到是这里。
fn repr_model_of(k: &ProviderKey) -> Option<String> {
    k.default_model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| k.models.first().map(|m| m.real_name.clone()))
}

/// 算钱要的两样 Key 级事实：代表模型（历史格用）与倍率。Key 已离开配置时回落到墓碑。
fn pricing_facts(
    key: Option<&ProviderKey>,
    tomb: Option<&usage_keys::KeyFacts>,
) -> (Option<String>, Option<String>) {
    let repr = key.and_then(repr_model_of).or_else(|| tomb.and_then(|t| t.repr_model.clone()));
    let mult = key
        .and_then(|k| k.cost_multiplier.clone())
        .or_else(|| tomb.and_then(|t| t.multiplier.clone()));
    (repr, mult)
}

/// 累计估算花费（纳美元）相对预算（美元）的关系。返回 `(要回显给界面的预算, 是否超支)`。
///
/// 🔴 **预算无效一律视同未设**：`None` / `≤0` / 非有限（NaN/Inf）都返回 `(None, false)`，
/// 不让一个笔误（把预算填成 0）把每条 Key 都标成超支。同 `balance_gate` 那条
/// 「查不到 ≠ 为零」的口径：宁可不管，也不误伤。
/// **没花过钱（`cost_nano == None`）不算超支** —— 一条还没跑过请求的新 Key，哪怕设了预算也不该标红。
fn budget_status(cost_nano: Option<u64>, budget: Option<f64>) -> (Option<f64>, bool) {
    let Some(b) = budget.filter(|b| b.is_finite() && *b > 0.0) else {
        return (None, false);
    };
    // 达到即算超（`>=`）：预算的语义是「别超过这个数」，正好等于它已经该提醒了。
    let over = cost_nano.is_some_and(|c| c as f64 / 1e9 >= b);
    (Some(b), over)
}
/// 某分类下**累计估算花费已达/超预算**的 Key id 集合（路由降级用，`Store::candidates_for` 调）。
///
/// 放这里而不是 `store.rs`：那边在棘轮上余量为 0，而本模块本就持有「花费怎么算」的全部口径
/// （[`key_cost`] + [`budget_status`]，与用量页同一套）。现算不缓存、永远新鲜，不像余额要打网络。
///
/// 🔴 **无预算 Key 时必须廉价早退**（`category_has_budget`：一次 `config.read` + `any`，不 clone）。
/// 这是**每请求**路径（`candidates_for` → `proxy`），而下面的 `list_keys` 会 clone 全部 Key
/// + 用量明细 clone 全量。绝大多数用户不设预算，不早退 = 每请求白付这几笔。
pub(crate) fn over_budget_ids(store: &Store, category: CategoryType) -> std::collections::HashSet<String> {
    if !store.category_has_budget(category) {
        return std::collections::HashSet::new();
    }
    let mut usage: HashMap<String, Vec<TokenUsageByKey>> = HashMap::new();
    for r in store.token_usage_by_model().into_iter().filter(|r| r.category_id == category) {
        usage.entry(r.key_id.clone()).or_default().push(r);
    }
    store
        .list_keys(category)
        .into_iter()
        .filter(|k| k.budget_usd.is_some())
        .filter(|k| {
            let Some(rows) = usage.get(&k.id) else { return false };
            let (repr, mult) = pricing_facts(Some(k), None);
            let cost = key_cost(rows, repr.as_deref(), mult.as_deref()).total;
            budget_status(cost, k.budget_usd).1
        })
        .map(|k| k.id)
        .collect()
}
/// 按「分类 × Key」聚合的用量 **+ 按模型逐格的成本估算**。
///
/// **Key 已离开配置的行走墓碑**（[`usage_keys`]）：算钱要的倍率与代表模型只存在于 `ProviderKey` 里。
///
/// # 🔴 行集合 = 「配置里的 Key」∪「历史累计桶」（2026-09-09 用户实报「新加的 key 都看不见」）
///
/// 正常采集刻意不建零桶，于是一条新 Key 在拿到第一笔上游回报的 usage 之前**根本没有桶**
/// —— 那不是刷新不及时，手动点刷新、重启应用都一样看不到。并集必须在后端做：前端凭行数组
/// 无法区分「后端没这一行」与「页面还没刷新」。占位行带 `has_recorded_usage: false`，金额留空
/// （**不是 $0**，理由见那个字段的文档）。已删除 Key 的行与空 `key_id` 的旧聚合行原样保留。
pub(crate) fn rows(store: &Store) -> Vec<UsageCostRow> {
    // 顺带把「当前还活着的 Key 的算钱事实」记下来，并拿回合并后的全表（见 usage_keys 模块头）。
    // 一趟遍历同时拿墓碑要的事实与并集要的 `(分类, id)` 位置 —— 两份「哪些 Key 算活着」必须是同一份。
    let today = usage_keys::today_ms(chrono::Utc::now().timestamp_millis());
    let mut live_slots: Vec<(CategoryType, String)> = Vec::new();
    let mut live: BTreeMap<String, usage_keys::KeyFacts> = BTreeMap::new();
    for category in CategoryType::ALL {
        for k in store.list_keys(category) {
            live_slots.push((category, k.id.clone()));
            let facts = usage_keys::KeyFacts {
                name: k.name.clone(),
                repr_model: repr_model_of(&k),
                multiplier: k.cost_multiplier.clone(),
                seen_day_ms: today,
            };
            live.insert(k.id, facts);
        }
    }
    let facts = usage_keys::sync(Path::new(&store.config_path_display()), live);

    // 先装历史桶（它们带真实数字），再把「配置里有、但一次 usage 都没捕获过」的 Key 补成占位行。
    // 顺序不能反 —— 反了会让占位行覆盖掉真实数字。`BTreeMap` 顺带给出稳定的行序（否则表格会无故跳动）。
    let mut slots: BTreeMap<(CategoryType, String), Vec<TokenUsageByKey>> = BTreeMap::new();
    for r in store.token_usage_by_model() {
        slots.entry((r.category_id, r.key_id.clone())).or_default().push(r);
    }
    for slot in live_slots {
        slots.entry(slot).or_default(); // 只补缺、不覆盖：已有桶的那几格原样保留
    }
    slots
        .into_iter()
        .map(|((category_id, key_id), parts)| build_row(store, &facts, category_id, key_id, parts))
        .collect()
}
/// 一行 = 一个 `(分类, Key)` 下各模型格的合计 + 估价 + 成因 + 预算。
fn build_row(
    store: &Store,
    facts: &BTreeMap<String, usage_keys::KeyFacts>,
    category_id: CategoryType,
    key_id: String,
    parts: Vec<TokenUsageByKey>,
) -> UsageCostRow {
    // 占位行（还没捕获过 usage）没有任何格。
    let has_recorded_usage = !parts.is_empty();
    let key = store.get_key(&key_id);
    // Key 已离开配置 → 回落到墓碑（它活着时最后一次被记下的事实）。
    let tomb = if key.is_none() { facts.get(&key_id) } else { None };
    let (repr, mult) = pricing_facts(key.as_ref(), tomb);
    let cost = key_cost(&parts, repr.as_deref(), mult.as_deref());
    let usage = parts.iter().fold(TokenUsage::default(), |mut acc, p| {
        acc.add(&p.usage);
        acc
    });
    let unpriced = cost.parts.iter().find(|p| p.cost_nano.is_none());

    // 🔴 占位行与「全部算出来了」一样**没有成因**：`UnpricedReason` 的每一支都在回答「为什么算不出钱」
    // 并给一条可行动的出路，而占位行压根没有消耗可算 —— 报任何一支都是把用户送去改一个没问题的配置。
    // 有几格算不出时取 token 最多的那一格（排序已把它放在算不出那几格的最前面）。
    let unpriced_reason = if !has_recorded_usage {
        None
    } else {
        unpriced.map(|p| reason_for(p, &key_id, key.is_some()))
    };
    // 没有消耗就没有金额可言：`estimate_cost` 对全零 usage 会算出货真价实的 `Some(0)`，
    // 界面就显示 $0.0000 —— 而那读起来是「已经在用、而且免费」。
    let cost_nano = if has_recorded_usage { cost.total } else { None };
    let priced: Vec<&UsagePartCost> = cost.parts.iter().filter(|p| p.cost_nano.is_some()).collect();
    let pricing_source = if priced.is_empty() {
        PricingSource::Unknown
    } else if priced.iter().any(|p| p.pricing_source == PricingSource::Family) {
        PricingSource::Family
    } else {
        PricingSource::Exact
    };
    // 排序是花费从高到低，第一格算出来的就是「主要按它估」的那个模型。
    let priced_by_model = priced.first().and_then(|p| p.priced_as.clone());
    // 花费预算：只认活着的 Key（墓碑不存预算），预算无效或还没花过钱都不标超支。
    let (budget_usd, over_budget) = budget_status(cost_nano, key.as_ref().and_then(|k| k.budget_usd));
    UsageCostRow {
        category_id,
        // 聚合行（key_id 为空串）不算「已删除」——它压根没有 Key。
        key_deleted: key.is_none() && !key_id.is_empty(),
        key_name: key.as_ref().map(|k| k.name.clone()).or_else(|| tomb.map(|t| t.name.clone())),
        key_id,
        usage,
        has_recorded_usage,
        cost_nano,
        pricing_source,
        multiplier: mult.unwrap_or_else(|| "1.0".into()),
        unpriced_reason,
        priced_by_model,
        breakdown: cost.parts,
        budget_usd,
        over_budget,
    }
}
/// 一格为什么算不出。顺序 = 从「有名可查但表里没有」到「连名字都没有」，每一支都排除了下一支，
/// 所以不会答出误导性的成因（四种成因的可行动性各不相同，见模块头那张表）。
fn reason_for(p: &UsagePartCost, key_id: &str, key_alive: bool) -> UnpricedReason {
    match p.priced_as.as_deref() {
        // 有名字可查（真实模型名，或历史格的代表模型），只是单价表里没有它 → 点出这个名字。
        Some(m) => UnpricedReason::ModelNotInTable { model: m.to_string() },
        // 以下三支都是「没记模型的历史格，且找不到代表模型」。
        None if key_id.is_empty() => UnpricedReason::Aggregate,
        None if !key_alive => UnpricedReason::KeyDeleted,
        None => UnpricedReason::NoModelName,
    }
}

/// 某一天的估算花费（用量页「今日 / 本周 / 本月」与趋势图）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DailyCost {
    /// 日期 `YYYY-MM-DD`，与日桶同口径（v4 起本机时区，见 `usage_store::local_date_string`）。
    pub(crate) date: String,
    /// 那天能算出的那几格之和（纳美元）。
    pub(crate) cost_nano: u64,
    /// 那天的 token 合计（四项之和，同用量页「合计」列）。
    pub(crate) tokens: u64,
    /// 其中**算不出金额**的 token 数。非 0 时界面必须标「部分未计入」——
    /// 否则「今日 $1.20」会被当成全部，而其实有一部分没算进去。
    pub(crate) unpriced_tokens: u64,
}

/// 按天的估算花费。**与 [`rows`] 同一套估价**（同一个 [`part_cost`]、同一份代表模型与倍率回落），
/// 只是聚合维度换成了「天」—— 两套算法的话「今日花费」与表格里的金额必然对不上。
///
/// 只读墓碑、不写（写由 [`rows`] 负责）；不在日桶里的天不出现（前端按「那天为 0」处理）。
pub(crate) fn daily_costs(store: &Store) -> Vec<DailyCost> {
    let facts = usage_keys::read_facts(Path::new(&store.config_path_display()));
    // 每条 Key 的代表模型与倍率只取一次（日桶里同一条 Key 会出现几十次）。
    let mut per_key: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    let mut facts_of = |kid: &str| {
        per_key
            .entry(kid.to_string())
            .or_insert_with(|| {
                let key = store.get_key(kid);
                let tomb = if key.is_none() { facts.get(kid) } else { None };
                pricing_facts(key.as_ref(), tomb)
            })
            .clone()
    };
    store
        .daily_usage_buckets()
        .into_iter()
        .map(|b| {
            let mut day = DailyCost { date: b.date, cost_nano: 0, tokens: 0, unpriced_tokens: 0 };
            for r in &b.entries {
                let (repr, mult) = facts_of(&r.key_id);
                let p = part_cost(r, repr.as_deref(), mult.as_deref());
                let n = token_sum(&r.usage);
                day.tokens = day.tokens.saturating_add(n);
                match p.cost_nano {
                    Some(c) => day.cost_nano = day.cost_nano.saturating_add(c),
                    None => day.unpriced_tokens = day.unpriced_tokens.saturating_add(n),
                }
            }
            day
        })
        .collect()
}
/// 删掉一条 Key 的**全部**用量记录与它的墓碑。不可逆。
///
/// 两个入口：`Store::delete_key`（删 Key 时一并删，2026-10-12 用户定的口径），以及用量页上对
/// 「（已删除）」行的清理按钮（本版本之前删掉的 Key、或绕过删除按钮离开配置的 Key）。
///
/// 🔴 **墓碑只在用量真的落了盘之后才删**：反过来的话，写盘失败 → 重启后用量复活、而算钱的事实
/// 已经没了，那一行就从「（已删除）但金额照算」退化成「—」。返回值 = 磁盘上是否也已删掉。
pub(crate) fn purge_key(store: &Store, key_id: &str) -> bool {
    let persisted = store.usage.purge_key(key_id);
    if persisted {
        usage_keys::forget(Path::new(&store.config_path_display()), key_id);
    }
    persisted
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

    /// 在**有过消耗的行**上：`unpriced_reason.is_some()` ⇔ 有某一格算不出；`cost_nano.is_none()` ⟹ 有成因。
    ///
    /// 两个字段各自独立赋值，很容易一边改了另一边没改 —— 而漂移的表现是静默的：
    /// 界面既显示了金额、又在横幅里把它计进「算不出的条数」，或者反过来显示「—」却不给成因。
    ///
    /// ⚠️ 按模型估价之后**不再是严格等价**（2026-10-12）：部分格算出、部分没有时两者都有
    /// （界面标「部分未计入」），故判据落在「格」上。占位行两者**同时**为 None，那是刻意的 ——
    /// 它压根没有消耗；下面另有一条专测占位行的用例钉住这个组合。
    #[test]
    fn reason_is_present_exactly_when_some_part_is_unpriced() {
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

        // 一条部分算得出的：同一条 Key 上一个表里有的模型 + 一个没有的。
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "half".into();
        store.upsert_key(k).unwrap();
        let u = crate::upstream::TokenUsage { input: 1000, output: 100, ..Default::default() };
        store.usage.record(CategoryType::ClaudeCli, "half", "claude-sonnet-4-5", &u);
        store.usage.record(CategoryType::ClaudeCli, "half", "totally-unknown-llm-9000", &u);

        for r in rows(&store).into_iter().filter(|r| r.has_recorded_usage) {
            let some_part_unpriced = r.breakdown.iter().any(|p| p.cost_nano.is_none());
            assert_eq!(
                some_part_unpriced,
                r.unpriced_reason.is_some(),
                "key_id={} 成因与逐格情况不一致：reason={:?} breakdown={:?}",
                r.key_id,
                r.unpriced_reason,
                r.breakdown
            );
            assert!(r.cost_nano.is_some() || r.unpriced_reason.is_some(), "key_id={} 显示「—」却不给成因", r.key_id);
            // 能算出来的行必须回显「按哪个模型算的」，否则用户看不出偏差来自哪。
            assert_eq!(
                r.cost_nano.is_some(),
                r.priced_by_model.is_some(),
                "能算出金额就必须回显代表模型"
            );
        }

        let all = rows(&store);
        assert_eq!(
            all.iter().filter(|r| r.cost_nano.is_some()).count(),
            2,
            "good 与 half 都必须能算出金额（若少了，说明单价表回归了）"
        );
        let half = all.iter().find(|r| r.key_id == "half").unwrap();
        assert!(
            half.cost_nano.is_some() && half.unpriced_reason.is_some(),
            "部分算得出 = 金额与成因同时存在（界面标「部分未计入」），不能退化成整行「—」"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **用户 2026-10-12 定的口径**：「key 删除的时候 对应的记录也可以删除了」。
    ///
    /// 推翻 09-02 的「删了 Key 用量照看、金额照算」。删掉之后：表里没有这一行、`usage.json` 里没有、
    /// 墓碑表里也没有；而别的 Key 一笔都不能少。故障注入：把 `delete_key` 里那行 `purge_key` 删掉 →
    /// 本测试变红（那一行会以「（已删除）」的形态留在表上，正是旧口径）。
    #[test]
    fn deleting_a_key_also_deletes_its_usage_records() {
        let (store, dir) = temp_store("delete_purges");
        let cfg = std::path::PathBuf::from(store.config_path_display());
        for (id, model) in [("doomed", "claude-sonnet-4-5"), ("keeper", "claude-opus-5-5")] {
            let mut k = key(CategoryType::ClaudeCli);
            k.id = id.into();
            k.default_model = Some(model.into());
            store.upsert_key(k).unwrap();
            let u = crate::upstream::TokenUsage { input: 1000, output: 100, ..Default::default() };
            store.append_event_model(CategoryType::ClaudeCli, "route", Some(id), "转发", None, None, Some(u), model);
        }
        let _ = rows(&store); // Key 活着时顺带写下墓碑
        assert!(store.flush_usage_if_dirty(), "前提：两条 Key 的用量都已落盘");
        let keys_file = usage_keys::file_path(&cfg);
        assert!(std::fs::read_to_string(&keys_file).unwrap().contains("\"doomed\""), "前提：墓碑在");

        store.delete_key("doomed").unwrap();

        let after = rows(&store);
        assert!(after.iter().all(|r| r.key_id != "doomed"), "删 Key 之后表里不许还有它（不是标「已删除」）");
        let usage_file = std::fs::read_to_string(crate::usage_store::usage_file_path(&cfg)).unwrap();
        assert!(!usage_file.contains("\"doomed\""), "usage.json 里也不许有（不能等 60s 的 flush）");
        assert!(!std::fs::read_to_string(&keys_file).unwrap().contains("\"doomed\""), "墓碑一并删掉");
        let keeper = after.iter().find(|r| r.key_id == "keeper").expect("别的 Key 必须还在");
        assert_eq!((keeper.usage.input, keeper.usage.output), (1000, 100), "别的 Key 一笔都不能少");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 绕过删除按钮离开配置的 Key（导入时整份替换 / 手改 config.json）：用量还在，靠墓碑照旧算钱并标
    /// 「（已删除）」；用户在用量页上点清理（[`purge_key`]）之后才连同墓碑一起消失。
    #[test]
    fn a_key_removed_outside_the_delete_button_keeps_its_price_until_purged() {
        let (store, dir) = temp_store("orphan_tomb");
        let cfg = std::path::PathBuf::from(store.config_path_display());
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "orphan".into();
        k.name = "被导入顶掉的站".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        k.cost_multiplier = Some("2.0".into());
        store.upsert_key(k).unwrap();
        // 升级前的历史用量（没记模型）→ 只能按代表模型估。
        let u = crate::upstream::TokenUsage { input: 1000, output: 100, ..Default::default() };
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("orphan"), "转发", None, None, Some(u));
        let priced = rows(&store).into_iter().find(|r| r.key_id == "orphan").unwrap().cost_nano.expect("活着时算得出");

        // 外部改写 config.json 把它拿掉（Store 的读前自愈会重新加载）。
        let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        json["keys"].as_array_mut().unwrap().retain(|k| k["id"] != "orphan");
        std::fs::write(&cfg, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

        let gone = rows(&store).into_iter().find(|r| r.key_id == "orphan").expect("没经过删除按钮 → 用量还在");
        assert!(store.get_key("orphan").is_none(), "前提：它确实离开了配置");
        assert!(gone.key_deleted, "要标出「已不在配置里」，否则它与正常在用的 Key 长得一样");
        assert_eq!(gone.cost_nano, Some(priced), "金额与离开配置之前完全一致（墓碑里的代表模型 + 倍率）");
        assert_eq!(gone.multiplier, "2.0", "倍率从墓碑还原，否则按 1.0 缩水");
        assert_eq!(gone.key_name.as_deref(), Some("被导入顶掉的站"));
        assert_eq!(gone.pricing_source, PricingSource::Family, "没记模型的历史格按代表模型估 → 必须打「≈」");

        assert!(purge_key(&store, "orphan"), "用户点清理：可写时必须真的落盘");
        assert!(rows(&store).iter().all(|r| r.key_id != "orphan"), "清理之后这一行消失");
        assert!(!usage_keys::read_facts(&cfg).contains_key("orphan"), "墓碑也一并删掉");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **本轮「用量统计不对」的根因**：同一条 Key 跑两个单价差几十倍的模型，必须各按各的价算。
    ///
    /// 旧口径按 Key 的代表模型（这里是便宜的 luna）估整条 Key，于是 astra 那 90% 的消耗被按 luna 的价
    /// 算 —— 实测用户一条 Key 输入低估约 50 倍。故障注入：让 `part_cost` 一律用 `repr` 查价 → 本测试变红。
    #[test]
    fn each_model_on_a_key_is_priced_at_its_own_rate() {
        let (store, dir) = temp_store("per_model_price");
        let mut k = key(CategoryType::Codex);
        k.id = "mixed".into();
        k.default_model = Some("gpt-5.6-luna".into()); // 代表模型 = 便宜的那个
        k.budget_usd = Some(5.0);
        store.upsert_key(k).unwrap();
        let big = crate::upstream::TokenUsage { input: 900_000, output: 90_000, ..Default::default() };
        let small = crate::upstream::TokenUsage { input: 100_000, output: 10_000, ..Default::default() };
        store.append_event_model(CategoryType::Codex, "route", Some("mixed"), "转发", None, None, Some(big), "gpt-6-astra");
        store.append_event_model(CategoryType::Codex, "route", Some("mixed"), "转发", None, None, Some(small), "gpt-5.6-luna");

        let row = rows(&store).into_iter().find(|r| r.key_id == "mixed").unwrap();
        let price = |m: &str, u: &crate::upstream::TokenUsage| crate::pricing::estimate_cost(u, Some(m), None).0.unwrap();
        let expected = price("gpt-6-astra", &big) + price("gpt-5.6-luna", &small);
        assert_eq!(row.cost_nano, Some(expected), "金额 = 各模型按自己的价算再相加");
        let as_repr = price("gpt-5.6-luna", &crate::upstream::TokenUsage { input: 1_000_000, output: 100_000, ..Default::default() });
        assert!(expected > as_repr * 20, "前提：两种算法差得足够远，本断言才有判别力");
        assert_eq!(row.breakdown.len(), 2, "逐模型拆分要带给界面");
        assert_eq!(row.breakdown[0].model, "gpt-6-astra", "花费最高的排第一");
        assert_eq!(row.priced_by_model.as_deref(), Some("gpt-6-astra"), "「主要按谁估」= 花费最多的那个");
        assert_eq!(row.pricing_source, PricingSource::Exact, "两个都是具体型号 → 不打「≈」");
        assert_eq!((row.usage.input, row.usage.output), (1_000_000, 100_000), "行上的 token 是各格之和");

        // 预算与用量页同一套算法：约 $13.5 的真实花费超了 $5 预算；按代表模型估只有 $0.32，会漏报。
        assert!(row.over_budget, "用量页必须标红");
        assert!(over_budget_ids(&store, CategoryType::Codex).contains("mixed"), "路由降级必须与用量页同口径");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 「今日花费」与表格金额**同一套估价**：今天只有一笔消耗时两边必须相等；算不出的那部分要单独报出来。
    #[test]
    fn daily_cost_matches_the_table_and_reports_the_unpriced_part() {
        let (store, dir) = temp_store("daily_cost");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "today".into();
        k.cost_multiplier = Some("0.5".into());
        store.upsert_key(k).unwrap();
        let priced = crate::upstream::TokenUsage { input: 20_000, output: 2_000, cache_read: 50_000, cache_creation: 0 };
        let unknown = crate::upstream::TokenUsage { input: 300, output: 30, ..Default::default() };
        store.usage.record(CategoryType::ClaudeCli, "today", "claude-opus-5-5", &priced);
        store.usage.record(CategoryType::ClaudeCli, "today", "totally-unknown-llm-9000", &unknown);

        let today = crate::usage_store::local_date_string(chrono::Utc::now().timestamp_millis());
        let days = daily_costs(&store);
        let d = days.iter().find(|d| d.date == today).expect("今天的未落盘消耗必须并进当天");
        let row = rows(&store).into_iter().find(|r| r.key_id == "today").unwrap();
        assert_eq!(Some(d.cost_nano), row.cost_nano, "今日花费 ≠ 表格金额 → 两套算法漂移了（倍率也要同样生效）");
        assert_eq!(d.tokens, 72_000 + 330, "tokens 是四项之和（同「合计」列）");
        assert_eq!(d.unpriced_tokens, 330, "算不出的那部分必须单独报，界面才能标「部分未计入」");

        // 落盘之后再看一次：同一份数字，不能因为从「未落盘增量」变成「已落盘的桶」而变化。
        assert!(store.flush_usage_if_dirty());
        let again = daily_costs(&store);
        assert_eq!(again.iter().find(|x| x.date == today), Some(d), "落盘前后同一天的数字必须一致");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn protected_usage_keys_files_are_not_overwritten() {
        use std::collections::BTreeMap;
        let (_store, dir) = temp_store("usage_keys_protected");
        let config = dir.join("config.json");
        let path = usage_keys::file_path(&config);
        let fact = |name: &str| usage_keys::KeyFacts {
            name: name.into(),
            repr_model: Some("claude-sonnet-4-5".into()),
            multiplier: Some("1.0".into()),
            seen_day_ms: 1,
        };
        let live = BTreeMap::from([(String::from("live"), fact("live"))]);

        std::fs::write(&path, br#"{"version":9999,"keys":{"old":{"name":"old","seenDayMs":1}}}"#).unwrap();
        let before_future = std::fs::read(&path).unwrap();
        let visible = usage_keys::sync(&config, live.clone());
        assert_eq!(std::fs::read(&path).unwrap(), before_future, "未来版本不得被旧程序覆盖");
        assert!(visible.contains_key("live"), "活 Key 仍应能在本次页面展示");

        std::fs::write(&path, b"{not-json").unwrap();
        let before_corrupt = std::fs::read(&path).unwrap();
        let _ = usage_keys::sync(&config, live);
        assert_eq!(std::fs::read(&path).unwrap(), before_corrupt, "损坏文件不得被空表覆盖");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_keys_atomic_write_failure_can_retry_without_losing_live_facts() {
        use std::collections::BTreeMap;
        let (_store, dir) = temp_store("usage_keys_retry");
        let config = dir.join("config.json");
        let path = usage_keys::file_path(&config);
        let live = BTreeMap::from([(String::from("retry"), usage_keys::KeyFacts {
            name: "retry".into(), repr_model: Some("claude-sonnet-4-5".into()),
            multiplier: None, seen_day_ms: 1,
        })]);
        std::fs::create_dir_all(&path).unwrap();
        let _ = usage_keys::sync(&config, live.clone());
        std::fs::remove_dir_all(&path).unwrap();
        let _ = usage_keys::sync(&config, live);
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["keys"]["retry"]["name"], "retry");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_usage_keys_syncs_merge_instead_of_overwriting() {
        use std::collections::BTreeMap;
        let (_store, dir) = temp_store("usage_keys_concurrent");
        let config = dir.join("config.json");
        let a = config.clone();
        let b = config.clone();
        let t1 = std::thread::spawn(move || usage_keys::sync(&a, BTreeMap::from([(String::from("a"), usage_keys::KeyFacts {
            name: "a".into(), repr_model: None, multiplier: None, seen_day_ms: 1,
        })])));
        let t2 = std::thread::spawn(move || usage_keys::sync(&b, BTreeMap::from([(String::from("b"), usage_keys::KeyFacts {
            name: "b".into(), repr_model: None, multiplier: None, seen_day_ms: 1,
        })])));
        let _ = t1.join().unwrap();
        let _ = t2.join().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(usage_keys::file_path(&config)).unwrap()).unwrap();
        assert!(json["keys"].get("a").is_some());
        assert!(json["keys"].get("b").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// 🔴 **用户报的问题**：「新加的 key 都看不见」。
    ///
    /// 修之前行集合是 `usage_totals` 的投影，而正常采集刻意不建零桶 → 一条还没产生过
    /// 可计量用量的 Key **根本没有行**，手动刷新、重启都一样看不到（不是刷新不及时）。
    ///
    /// 故障注入判据：把 `rows()` 里补占位行的那段 `or_insert_with` 删掉 → 本测试变红。
    #[test]
    fn a_new_key_shows_up_before_it_has_any_usage() {
        let (store, dir) = temp_store("new_key_row");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "fresh".into();
        k.name = "刚加的站".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        store.upsert_key(k).unwrap();
        // 刻意**不**产生任何用量事件。

        let rows = rows(&store);
        let fresh = rows
            .iter()
            .find(|r| r.key_id == "fresh")
            .expect("刚加的 Key 必须立刻出现在用量表里，哪怕它还没跑过一次请求");
        assert!(!fresh.has_recorded_usage, "还没捕获过 usage → 这一位必须是 false");
        assert_eq!(fresh.key_name.as_deref(), Some("刚加的站"));
        assert_eq!(
            (fresh.usage.input, fresh.usage.output),
            (0, 0),
            "占位行的 token 数是 0"
        );
        // 🔴 金额必须是「—」而不是 $0：后者读起来是「已经在用、而且免费」。
        assert_eq!(fresh.cost_nano, None, "没有消耗就没有金额，绝不能算成 Some(0)");
        // 也不许给「算不出金额」的成因 —— 那些每一支都在指挥用户去改配置，
        // 而这里没有任何配置需要改。
        assert_eq!(
            fresh.unpriced_reason, None,
            "占位行不该有成因，否则告警条会催用户去修一个没坏的东西"
        );
        assert!(!fresh.key_deleted, "它当然还在");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 占位行在**第一笔用量到达后**必须转成真实行，而不是多出一行。
    ///
    /// 这是并集实现最容易写错的地方：若并集的键不是 `(分类, key_id)` 而只是 `key_id`，
    /// 或者补占位时覆盖了已有桶，就会出现「同一条 Key 两行」或「数字被清零」。
    #[test]
    fn the_placeholder_row_turns_into_the_real_one_instead_of_duplicating() {
        let (store, dir) = temp_store("placeholder_promote");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "warmup".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        store.upsert_key(k).unwrap();

        assert_eq!(
            rows(&store).iter().filter(|r| r.key_id == "warmup").count(),
            1,
            "前提：占位阶段恰好一行"
        );

        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("warmup"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 1000, output: 100, ..Default::default() }),
        );

        let rows_now = rows(&store);
        let mine: Vec<&UsageCostRow> = rows_now.iter().filter(|r| r.key_id == "warmup").collect();
        assert_eq!(mine.len(), 1, "第一笔用量到达后仍必须只有一行，不能并出第二行");
        let row = mine[0];
        assert!(row.has_recorded_usage, "有了真实消耗 → 这一位翻真");
        assert_eq!((row.usage.input, row.usage.output), (1000, 100), "数字必须是真实值");
        assert!(row.cost_nano.is_some(), "有消耗且模型在表里 → 必须算出金额");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 并集**不得吃掉历史行**：已删除 Key 的行与空 `key_id` 的旧聚合行都要留着。
    ///
    /// 用量是纯累加值，历史不该因为「这条 Key 现在不在配置里」而消失 ——
    /// 那等于让用户的累计花费凭空变小（本仓在 90 天滚动删桶上修过同族缺陷）。
    #[test]
    fn the_union_never_drops_rows_that_have_no_live_key() {
        let (store, dir) = temp_store("union_history");
        // ① 一条活着的 Key（会有占位行）
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "alive".into();
        store.upsert_key(k).unwrap();
        // ② 指向已删除 Key 的历史行
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("ghost"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 20, output: 2, ..Default::default() }),
        );
        // ③ 空 key_id 的旧聚合行
        store.append_event_full(
            CategoryType::Codex,
            "aggregate",
            None,
            "聚合",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 10, output: 1, ..Default::default() }),
        );

        let rows = rows(&store);
        let ghost = rows.iter().find(|r| r.key_id == "ghost").expect("已删 Key 的历史行必须保留");
        assert!(ghost.has_recorded_usage, "它有真实消耗");
        assert!(ghost.key_deleted, "且要标出「已删除」");
        let agg = rows.iter().find(|r| r.key_id.is_empty()).expect("旧聚合行必须保留");
        assert!(agg.has_recorded_usage);
        assert_eq!(agg.unpriced_reason, Some(UnpricedReason::Aggregate), "成因不受并集影响");
        assert!(rows.iter().any(|r| r.key_id == "alive"), "活着的 Key 也在");

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

    /// 🔴 **用户要的功能**：给 Key 设花费预算，累计估算花费到顶就标红/告警。
    ///
    /// 纯函数边界 + 端到端各覆盖一半：`budget_status` 判无效预算/未花钱不误伤，
    /// `rows()` 把它接到真实 Key 上。故障注入：把 `over` 恒置 false → e2e 变红；
    /// 把预算无效那道门去掉（`Some(0.0)` 也当有效）→ 边界断言变红。
    #[test]
    fn a_budget_flags_the_row_once_cumulative_cost_reaches_it() {
        // 纯函数：达到即超、没花过钱不超、无效预算视同未设。
        assert_eq!(budget_status(Some(5_000_000_000), Some(3.0)), (Some(3.0), true), "$5 ≥ $3");
        assert_eq!(budget_status(Some(2_000_000_000), Some(3.0)), (Some(3.0), false), "$2 < $3");
        assert_eq!(budget_status(Some(3_000_000_000), Some(3.0)), (Some(3.0), true), "达到即算超");
        assert_eq!(budget_status(None, Some(3.0)), (Some(3.0), false), "还没花过钱不算超");
        assert_eq!(budget_status(Some(9_000_000_000), None), (None, false), "没设预算不触发");
        assert_eq!(budget_status(Some(9_000_000_000), Some(0.0)), (None, false), "预算 0 视同未设");
        assert_eq!(budget_status(Some(9_000_000_000), Some(-1.0)), (None, false), "负预算视同未设");
        assert_eq!(budget_status(Some(9_000_000_000), Some(f64::NAN)), (None, false), "NaN 视同未设");

        // 端到端：设了 $0.001 的极低预算，跑一笔真实用量后必被标红并回显预算值。
        let (store, dir) = temp_store("budget_flag");
        let mut k = key(CategoryType::ClaudeCli);
        k.id = "capped".into();
        k.default_model = Some("claude-sonnet-4-5".into());
        k.budget_usd = Some(0.001);
        store.upsert_key(k).unwrap();
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("capped"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 100_000, output: 10_000, ..Default::default() }),
        );
        let row = rows(&store).into_iter().find(|r| r.key_id == "capped").unwrap();
        assert_eq!(row.budget_usd, Some(0.001), "预算值要回显给界面画进度条");
        assert!(row.over_budget, "累计花费已超过 $0.001 预算，必须标红");

        // 占位行（设了预算但还没花过钱）不许标红。
        let mut fresh = key(CategoryType::ClaudeCli);
        fresh.id = "fresh".into();
        fresh.budget_usd = Some(1.0);
        store.upsert_key(fresh).unwrap();
        let frow = rows(&store).into_iter().find(|r| r.key_id == "fresh").unwrap();
        assert_eq!(frow.budget_usd, Some(1.0), "未花钱也要回显预算（进度条显示 $0 / $1）");
        assert!(!frow.over_budget, "还没产生花费的新 Key 不该被标成超预算");

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
