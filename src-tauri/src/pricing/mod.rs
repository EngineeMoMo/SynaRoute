//! 模型单价查表与成本核算。数据表在 [`table`]，这里只放逻辑。
//!
//! 关键设计：
//! - 成本全部用**定点数运算**（单位：纳美元 / token）避免浮点累加误差。
//! - 查表是「**归一 + 最长片段命中**」，表序不参与语义（见 `table` 的模块注释）。
//! - 用户可为中转商 Key 配置**倍率**（如 `0.3` 表示三折），按 Key 乘上去。
//!
//! Token → 美元换算链路：
//! ```text
//! TokenUsage {input, output, cache_read, cache_creation}
//!   ↓ lookup_row()：归一模型名 → 最长片段命中一行
//!   ↓ 各项乘对应单价（缓存价按行里的显式值，缺省才用 0.1× / 1.25×）
//!   ↓ 四项求和 → 纳美元
//!   ↓ 乘 Key 倍率 → 最终成本
//!   ↓ format_usd_from_nano() → "$0.0123"（显示用）
//! ```

pub(crate) mod table;

use serde::{Deserialize, Serialize};
use table::{Row, FAMILY_FLAGSHIP, PRICING_TABLE};

pub(crate) use table::PRICE_TABLE_VERIFIED_ON;

/// 单个模型的单价结构（单位：**纳美元 / token**，即 1e-9 USD）。
///
/// 举例：`input_per_ntx: 3000` = 每 token 3000 n$ = $0.000003（即每百万 token $3）。
///
/// **为什么用纳美元而不是微美元**：微美元精度下 `$3/MTok` 只能存成 `3`，
/// 而缓存命中价 = 输入价 ÷ 10 → `3/10` 整数除法直接归零，等于缓存不计费。
/// 纳美元下是 `3000/10 = 300`，精度足够。这是实测（`cache_read_per_mtx == 0`）
/// 才发现的：一个纯粹的单位选择失误会让整类 token 静默免费。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPricing {
    /// 输入单价（nano-USD per token）
    pub input_per_ntx: u64,
    /// 输出单价
    pub output_per_ntx: u64,
    /// 缓存命中单价
    pub cache_read_per_ntx: u64,
    /// 缓存创建单价
    pub cache_creation_per_ntx: u64,
}

/// $X/MTok → 纳美元/token（X × 1000，四舍五入）。
fn mtok_to_ntx(per_mtok: f64) -> u64 {
    (per_mtok * 1000.0).round() as u64
}

impl ModelPricing {
    /// 从官方单价（$/MTok）构造，缓存价按默认比例推导。
    ///
    /// 默认比例（0.1× 读 / 1.25× 写）只对 Anthropic、gpt-5 族、Gemini 成立。
    /// 其余厂商在表里显式给 `cache_read`，走 [`Self::from_row`]。
    pub fn from_official(input_per_mtok: f64, output_per_mtok: f64) -> Self {
        let input = mtok_to_ntx(input_per_mtok);
        Self {
            input_per_ntx: input,
            output_per_ntx: mtok_to_ntx(output_per_mtok),
            cache_read_per_ntx: input / 10,
            cache_creation_per_ntx: input + input / 4, // 1.25 倍，避免浮点：x + x/4
        }
    }

    /// 从表里的一行构造：显式给了缓存价就用它，否则退回默认比例。
    fn from_row(row: &Row) -> Self {
        let mut p = Self::from_official(row.input, row.output);
        if let Some(cr) = row.cache_read {
            p.cache_read_per_ntx = mtok_to_ntx(cr);
        }
        if let Some(cw) = row.cache_write {
            p.cache_creation_per_ntx = mtok_to_ntx(cw);
        }
        p
    }

    /// 计算一次请求的成本（**纳美元**），含 Key 倍率（如 0.3 表示三折）。
    ///
    /// 用 u128 中间累加：单次请求 token 数 × 单价在 u64 下有溢出余量，但按日/按月
    /// 汇总时会累加成千上万次，u128 给足空间。
    pub fn calculate_cost_nano(&self, usage: &crate::model::TokenUsage, multiplier: f64) -> u64 {
        let base = usage.input as u128 * self.input_per_ntx as u128
            + usage.output as u128 * self.output_per_ntx as u128
            + usage.cache_read as u128 * self.cache_read_per_ntx as u128
            + usage.cache_creation as u128 * self.cache_creation_per_ntx as u128;
        // 倍率是用户填的小数（0.3 之类），这一步无法避免浮点；但它只作用于**最终和**，
        // 不参与逐项累加，故不会有误差累积。
        (base as f64 * multiplier).round() as u64
    }
}

/// 格式化 nano-USD 为美元字符串，保留 4 位小数（如 `63_000_000` → `"$0.0630"`）。
///
/// 4 位而非 6 位：金额面板上 `$0.012345` 这种精度对用户无意义，反而挤占版面。
/// 内部一律按纳美元精确累加，只在**显示**这一步收敛精度。
///
/// **当前只被测试用到**：金额是在前端渲染的（`UsagePage.tsx` 有一份同口径的 `fmtUsd`），
/// IPC 传的是原始 `costNano` 整数 —— 传数值而非预格式化字符串，
/// 前端才能自己决定精度、做小计与排序。
///
/// 保留它有两个用途：① 钉住「4 位小数 + 四舍五入」这个显示口径，前端那份实现
/// 要与它对齐（测试是唯一的对照物）；② 后端日后要在日志/诊断报告里打印金额时直接可用。
#[cfg_attr(not(test), allow(dead_code))]
pub fn format_usd_from_nano(nano: u64) -> String {
    // 纳美元 → 万分之一美元（4 位小数）：÷ 1e5，四舍五入
    let ten_thousandths = (nano + 50_000) / 100_000;
    format!("${}.{:04}", ten_thousandths / 10_000, ten_thousandths % 10_000)
}

/// 归一模型名：小写 + `.`/`/`/`_` → `-`。
///
/// 三种分隔符各有真实来源：`.` 是中转站写法（`glm-4.6`、`gpt-5.6-sol`）；
/// `/` 是 OpenRouter 式前缀（`anthropic/claude-opus-5`）；`_` 少数中转在用。
/// `upstream/budget.rs` 的 `normalize_model_segments` 是同一条判据，两处别分叉。
///
/// 不归一的后果实测过：`gpt-4.1-nano` 命中 `gpt-4` 那一行，输入价高估 **100 倍**。
fn normalize(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['.', '/', '_'], "-")
}

/// 最长片段命中，**平局时按片段字符串排序**决出唯一胜者。
///
/// 两层设计各修一个真实缺陷：
///
/// 1. **最长优先**替代旧的「表序首个 contains」。旧实现里 `opus` 必须排在 `claude` 之前，
///    排错了不报错、金额只是悄悄变成 1/5。
/// 2. **平局按 `frag` 字典序**（而不是「表里谁先谁赢」）。没有这一层，
///    `gpt-5-codex` 会同时命中 `gpt-5` 与 `codex`（都是 5 个字符），
///    `max_by_key` 的平局规则是「取最后一个」，于是正序扫表得 `codex`、
///    倒序得 `gpt-5` —— 表序又偷偷回到语义里了。这个洞是
///    `pricing_is_independent_of_table_order` 抓出来的，不是想出来的。
///
/// 平局本身仍应尽量避免（靠给具体型号补显式行），因为字典序只保证**确定**、不保证**对**。
/// `no_ambiguous_fragment_ties_in_coverage` 那条测试把「覆盖名单里不许有平局」钉住。
fn lookup_row(model: &str) -> Option<&'static Row> {
    let n = normalize(model);
    if n.is_empty() {
        return None;
    }
    PRICING_TABLE
        .iter()
        .filter(|row| fragment_matches(row.frag, &n))
        .max_by_key(|row| (row.frag.len(), row.frag))
}

/// 片段是否命中模型名 —— 带**版本边界**判据。
///
/// # 为什么不能是裸 `contains`
///
/// 「以数字结尾的片段」同时是它自己后续小版本的前缀，于是最长命中会把**未来的小版本**
/// 钉死在最老的那个兄弟行上，而不是落到家族兜底（= 现役旗舰价）。实测三处：
///
/// | 模型名 | 裸 contains 命中 | 价 | 旗舰价 |
/// |---|---|---|---|
/// | `gpt-5.6` | `gpt-5` | $1.25/$10 | `gpt-5.6-sol` $4/$20（**2.1× 低**）|
/// | `glm-5.9` | `glm-5` | $1.00/$3.20 | `glm-5.3` $1.40/$4.40 |
/// | `gpt-4.5` | `gpt-4`（**`live: false`**）| $30/$60 | —— |
///
/// 而这三条的 `PricingSource` 都是 `Exact` → 界面**不打 `≈`**，用户看到一个自信的错数字。
/// 偏低尤其糟（模块头写明「宁可偏高」）：它让人不知不觉超支。
/// Claude 侧天然免疫 —— 那边家族行是具体行的**前缀**（`claude-opus` ⊂ `claude-opus-5`），
/// 方向恰好相反；GPT/GLM 是倒过来的，所以才只在这两族暴露。
///
/// # 判据
///
/// 片段以数字结尾、且模型名里紧跟 `-<数字>` 时，这一处出现**不算命中**（那是更晚的小版本）。
/// 其余一律照旧，故这些必须不受影响（都在下面的测试里）：
/// `gpt-4` ↛ `gpt-4-1-nano` 但 → `gpt-4o` / `gpt-4-turbo`；`o1` → `o1-mini`；
/// `deepseek-v4` → `deepseek-v4-pro`；`glm-4` → `glm-4-plus`。
///
/// 同一片段在名字里出现多次时逐处试（`from` 往后推），只要有一处是有效命中就算命中。
fn fragment_matches(frag: &str, normalized: &str) -> bool {
    let ends_with_digit = frag.as_bytes().last().is_some_and(u8::is_ascii_digit);
    let mut from = 0usize;
    while let Some(rel) = normalized[from..].find(frag) {
        let at = from + rel;
        let rest = &normalized[at + frag.len()..];
        // 「更晚的小版本」= 片段以数字结尾，且后面紧跟 `-` 再跟一个数字
        let is_later_minor = ends_with_digit
            && rest.as_bytes().first() == Some(&b'-')
            && rest.as_bytes().get(1).is_some_and(u8::is_ascii_digit);
        if !is_later_minor {
            return true;
        }
        from = at + 1;
    }
    false
}

/// 按模型名查单价。
///
/// **当前只被测试用到**（生产走 [`estimate_cost`]，它还要带倍率与来源）。保留它是因为
/// 「某个模型名查出来的四个价是多少」是这张表最直接的判据形态，测试里大量用到。
#[cfg_attr(not(test), allow(dead_code))]
pub fn lookup_pricing(model_name: &str) -> Option<ModelPricing> {
    lookup_row(model_name).map(ModelPricing::from_row)
}

/// 单价的来源，供界面如实标注精度。
///
/// 用户会拿这个面板的数字与中转站账单对比，所以**必须让他知道这数是怎么来的**：
/// 精确命中某个具体型号、还是按家族兜底猜的、还是压根没有单价。含糊其辞会让用户
/// 把估算当账单，对不上时以为程序算错了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PricingSource {
    /// 命中某个**具体型号**行（带世代号，如 `claude-opus-5`）。
    Exact,
    /// 命中**家族兜底**行（裸家族名，如 `claude-opus` / `gpt`）—— 按当前旗舰价估。
    Family,
    /// 没有任何单价可用 → 金额不可估算。
    Unknown,
}

/// 一行是不是「家族兜底行」：它出现在 [`FAMILY_FLAGSHIP`] 的左列。
///
/// 判据刻意用这张显式表，**不是**「片段里不含数字」那种启发式 ——
/// `claude` / `gpt` / `glm` 是兜底行，而 `hy3` 含数字却也是具体行、`gpt-oss` 不含数字
/// 但同时是兜底行。启发式在这两边都会判错，而判错的表现是界面上「≈」标反。
fn is_family_row(frag: &str) -> bool {
    FAMILY_FLAGSHIP.iter().any(|(family, _)| *family == frag)
}

/// 计费倍率的上界。超过即视为笔误、按 1.0 处理（见 [`estimate_cost`]）。
///
/// 1000 这个数字的取法：中转站的折扣区间实际是 0.1~5（常见 0.3、0.5、2），
/// 官方原价是 1.0。留三个数量级的余量已足够容纳任何真实定价，
/// 再大只可能是「少点一个小数点」或「把每百万 token 的价格当倍率填了进来」。
/// 前端也用同一个界做输入校验（`MAX_COST_MULTIPLIER`），两处必须同值。
pub const MAX_COST_MULTIPLIER: f64 = 1000.0;

/// 估算一段用量的成本（纳美元）+ 单价来源。
///
/// `model_hint` 传该 Key 的代表模型名（默认兜底模型或首个模型）。
/// **本函数刻意不按模型逐条算**：用量累加器的键是 `(分类, keyId)`、不含模型名，
/// 要按模型精确计费得把落盘格式再升一版。而用户选定的方案是「每个 Key 一个折扣倍率」，
/// 倍率本就挂在 Key 上，故按 Key 估算与该方案一致。代价是同一 Key 跑多个不同档位
/// 模型时会有偏差 —— 界面必须标明这是估算，并回显用的是哪个代表模型
/// （见 `usage_cost::UsageCostRow::priced_by_model`）。
pub fn estimate_cost(
    usage: &crate::model::TokenUsage,
    model_hint: Option<&str>,
    multiplier: Option<&str>,
) -> (Option<u64>, PricingSource) {
    // 倍率解析失败（用户填了 "abc"）时按 1.0，不让一个笔误把金额算成 0。
    //
    // `is_finite` 那道判必须有：`"inf"` 与 `"1e400"` 都能**成功**解析成 `f64::INFINITY`，
    // 而 `INFINITY > 0.0` 为真、能过下面那道门。乘出来的 f64 转 u64 在 Rust 里是饱和转换
    // （不是 UB），于是金额直接变成 u64::MAX —— 面板上显示 $18446744073.7 这种数字。
    // 上界 `MAX_COST_MULTIPLIER` 同理：中转站的折扣再离谱也不会到 1000 倍，
    // 填出这种值只会是笔误，而它把估算撑成天文数字后用户第一反应是「这程序坏了」。
    let mult = multiplier
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|m| m.is_finite() && *m > 0.0 && *m <= MAX_COST_MULTIPLIER)
        .unwrap_or(1.0);

    let Some(name) = model_hint.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, PricingSource::Unknown);
    };

    match lookup_row(name) {
        None => (None, PricingSource::Unknown),
        Some(row) => {
            let cost = ModelPricing::from_row(row).calculate_cost_nano(usage, mult);
            let src = if is_family_row(row.frag) {
                PricingSource::Family
            } else {
                PricingSource::Exact
            };
            (Some(cost), src)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TokenUsage;

    /// 覆盖名单：**每一条都必须能查到价**（Exact 或 Family）。
    ///
    /// 这份名单是实测跑出来的现实模型名。旧实现在这 100 条里有 58 条落 Unknown ——
    /// 包括整个 gpt-5 系、整个 o 系、codex 系、以及国内多家。
    /// 「Codex 分类的花费列恒为 —」有一半原因就在这里。
    const COVERAGE: &[&str] = &[
        // OpenAI
        "gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-5.1", "gpt-5.2", "gpt-5.6",
        "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.4",
        "gpt-5.4-mini", "gpt-5.4-nano", "gpt-5-codex", "gpt-5.1-codex",
        "gpt-5.1-codex-max", "gpt-5.1-codex-mini", "gpt-5.3-codex", "codex-mini-latest",
        "gpt-6", "o1", "o1-mini", "o3", "o3-mini", "o3-pro", "o4-mini",
        "gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano", "gpt-4o", "gpt-4o-mini",
        "gpt-4-turbo", "gpt-3.5-turbo", "chatgpt-4o-latest", "gpt-oss-120b", "gpt-oss-20b",
        // Anthropic
        "claude-opus-5", "claude-opus-5-thinking", "claude-opus-4-8", "claude-opus-4-6",
        "claude-opus-4-5", "claude-sonnet-5", "claude-sonnet-4-6", "claude-sonnet-4-5",
        "claude-haiku-4-5", "claude-fable-5", "claude-mythos-5",
        "claude-3-7-sonnet-20250219", "anthropic/claude-opus-5", "claude-synaroute-opus",
        // Google
        "gemini-3.7-flash", "gemini-3.5-flash", "gemini-3.1-pro-preview", "gemini-3-flash",
        "gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.5-flash-lite",
        // DeepSeek
        "deepseek-v4-pro", "deepseek-v4-flash", "deepseek-chat", "deepseek-reasoner",
        "deepseek-ai/DeepSeek-V3",
        // 智谱
        "glm-5.3", "glm-5", "glm-4.7", "glm-4.6", "glm-4.5-air", "glm-4-plus", "glm-4.7-flash",
        // Kimi
        "kimi-k3", "kimi-k2.6", "kimi-k2.7-code", "kimi-k2-thinking", "moonshot-v1-128k",
        // Qwen
        "qwen3.8-max", "qwen3.7-plus", "qwen3-max", "qwen3-coder-plus", "qwen-plus",
        "qwen-max", "qwen-turbo",
        // xAI
        "grok-4.6", "grok-4.5", "grok-4-fast", "grok-code-fast-1", "grok-3-mini", "grok-5",
        // 其余
        "mistral-large-latest", "mistral-medium", "codestral-latest", "ministral-3",
        "minimax-m3", "minimax-m2", "MiniMax-Text-01",
        "hy3", "hunyuan-turbos-latest", "seed-2-1-turbo",
        "command-a-03-2025", "command-r-plus", "sonar", "sonar-pro", "sonar-reasoning-pro",
        "llama-3.3-70b-instruct", "gemma-3-27b-it",
    ];

    /// 故障注入判据：删掉 `("gpt", …)` 那一行 → 本测试必须变红（gpt-5 系会全部落 Unknown）。
    #[test]
    fn coverage_list_never_falls_to_unknown() {
        let usage = TokenUsage { input: 1_000_000, output: 0, cache_read: 0, cache_creation: 0 };
        let mut missing = Vec::new();
        for m in COVERAGE {
            let (cost, src) = estimate_cost(&usage, Some(m), None);
            if cost.is_none() || src == PricingSource::Unknown {
                missing.push(*m);
            }
        }
        assert!(
            missing.is_empty(),
            "以下现实模型名查不到单价（界面会显示「—」）：{missing:?}"
        );
    }

    /// **表序不影响结果**。旧实现是「表序首个 contains」，顺序敏感 ——
    /// `opus` 排到 `claude` 之后，最贵的模型就按中档价算，而界面完全看不出错。
    ///
    /// 故障注入判据：把 `lookup_row` 的 `max_by_key` 换回 `find` → 本测试必须变红。
    /// 另一个注入：把平局键从 `(len, frag)` 改回 `len` → `gpt-5-codex` 必须变红。
    #[test]
    fn pricing_is_independent_of_table_order() {
        let usage = TokenUsage { input: 1_000, output: 1_000, cache_read: 1_000, cache_creation: 0 };
        for m in COVERAGE {
            let forward = estimate_cost(&usage, Some(m), None);
            // 用同一条规则重算一遍，但**倒序**扫表。结果必须逐条相同。
            let n = normalize(m);
            // **必须复用 `fragment_matches`**，不能写成裸 `n.contains(row.frag)`：
            // 那样这条测试比的就不是「同一规则的两个扫表方向」，而是「两套不同规则」，
            // 于是任何对匹配规则的改动都会让它变红 —— 一条会误报的判据等于噪音。
            let rev = PRICING_TABLE
                .iter()
                .rev()
                .filter(|row| fragment_matches(row.frag, &n))
                .max_by_key(|row| (row.frag.len(), row.frag))
                .map(|row| ModelPricing::from_row(row).calculate_cost_nano(&usage, 1.0));
            assert_eq!(forward.0, rev, "模型 {m} 的结果依赖了表的顺序");
        }
    }

    /// 覆盖名单里**不许有长度平局** —— 字典序只保证结果确定，不保证选对了那一行。
    ///
    /// 平局出现时正确的处置是给具体型号补一条更长的显式行（`gpt-5-codex` 就是这么修的），
    /// 而不是依赖字典序碰巧选对。这条测试让平局无法悄悄溜进来。
    #[test]
    fn no_ambiguous_fragment_ties_in_coverage() {
        let mut ties = Vec::new();
        for m in COVERAGE {
            let n = normalize(m);
            let mut hits: Vec<&Row> = PRICING_TABLE.iter().filter(|r| n.contains(r.frag)).collect();
            if hits.len() < 2 {
                continue;
            }
            hits.sort_by_key(|r| std::cmp::Reverse(r.frag.len()));
            if hits[0].frag.len() == hits[1].frag.len() {
                ties.push(format!("{m} → `{}` 与 `{}` 长度相同", hits[0].frag, hits[1].frag));
            }
        }
        assert!(
            ties.is_empty(),
            "存在长度平局的片段，胜者只由字典序决定（可能选错档位）：{ties:#?}"
        );
    }

    /// 「声称免费」与「表里真的是 0」不得分叉：`FREE_MODELS` 里的每个片段
    /// 都必须在表里有一条 input=output=0 的具体行。
    #[test]
    fn free_models_are_zero_rows_in_the_table() {
        for f in table::FREE_MODELS {
            let row = PRICING_TABLE
                .iter()
                .find(|r| r.frag == *f)
                .unwrap_or_else(|| panic!("免费片段 `{f}` 在表里没有对应行"));
            assert_eq!((row.input, row.output), (0.0, 0.0), "`{f}` 应是零价行");
        }
    }

    /// 每个家族兜底行的四个价必须**逐字**等于 `FAMILY_FLAGSHIP` 指向的那一行。
    ///
    /// 这条是本轮 3× 高估的守门人：旧表 `("opus", 15.0, 75.0)` 停在**已退役**的
    /// Opus 4/4.1 价上，而现役 4.5~5 是 5/25。
    ///
    /// 故障注入判据：把 `claude-opus` 兜底行改回 15/75 → 必须变红。
    #[test]
    fn family_default_equals_designated_flagship() {
        for (family, flagship) in FAMILY_FLAGSHIP {
            let fam = PRICING_TABLE
                .iter()
                .find(|r| r.frag == *family)
                .unwrap_or_else(|| panic!("FAMILY_FLAGSHIP 里的家族行 `{family}` 不在表里"));
            let ship = PRICING_TABLE
                .iter()
                .find(|r| r.frag == *flagship)
                .unwrap_or_else(|| panic!("旗舰行 `{flagship}` 不在表里"));
            let a = ModelPricing::from_row(fam);
            let b = ModelPricing::from_row(ship);
            assert_eq!(
                (a.input_per_ntx, a.output_per_ntx, a.cache_read_per_ntx, a.cache_creation_per_ntx),
                (b.input_per_ntx, b.output_per_ntx, b.cache_read_per_ntx, b.cache_creation_per_ntx),
                "家族兜底行 `{family}` 的价与它指定的旗舰 `{flagship}` 不一致 —— \
                 兜底行的语义是「该家族当前已知最好值」，停在旧价上会让所有新版本静默算错"
            );
        }
    }

    /// 旗舰指针不得指向**已退役**的行。
    ///
    /// 故障注入判据：把 `("claude-opus", "claude-opus-5")` 改成指向 `claude-opus-4-1` → 必须变红。
    #[test]
    fn flagship_row_must_be_live() {
        for (family, flagship) in FAMILY_FLAGSHIP {
            let ship = PRICING_TABLE.iter().find(|r| r.frag == *flagship).unwrap();
            assert!(
                ship.live,
                "家族 `{family}` 的旗舰指针指向已退役的 `{flagship}` —— \
                 兜底价会停在退役价上（这正是上一版 3× 高估的成因）"
            );
        }
    }

    /// 每个片段命中的名字必须全属同一个声明厂商。
    ///
    /// 防的是「短片段乱吃别家名字」：比如加一行 `("yi", …)` 会命中
    /// `gemini`（含 "i"…不会）——但 `("gpt", …)` 与 `("gpt-oss", …)` 这类真实的交叉
    /// 必须靠最长命中正确区分。这条测试把它变成机械判据。
    ///
    /// 故障注入判据：加一行 `r("on", Vendor::Cohere, …)` → 必须变红（会吃掉 sonar/sonnet 等）。
    #[test]
    fn no_fragment_matches_another_vendors_model() {
        for m in COVERAGE {
            let n = normalize(m);
            let hit = PRICING_TABLE
                .iter()
                .filter(|row| fragment_matches(row.frag, &n))
                .max_by_key(|row| (row.frag.len(), row.frag));
            let Some(hit) = hit else { continue };
            // 名字里含厂商强特征词时，命中的行必须属于那个厂商。
            //
            // ⚠️ **这张表原先漏掉了 OpenAI / o 系 / codex / Mistral / Cohere / 开放权重**，
            // 也就是说表里最大的那一族（OpenAI 约 35 行，Codex 分类就跑在它上面）
            // 完全不在判据覆盖内。实测两处真窟窿在当时都是绿的：
            // 加一行 `("codex-mini", Cohere)` 会吃掉 `gpt-5.1-codex-mini` 与 `codex-mini-latest`；
            // 加一行 `("3-70b-instruct", Zhipu)` 会吃掉 `llama-3.3-70b-instruct`。
            //
            // 🔴 **顺序在这里是语义的一部分**（`find` 取首个命中）：开放权重的三条必须排在
            // 裸 `gpt` 之前 —— `gpt-oss-120b` 合法地属于 `OpenWeight`，
            // 把 `("gpt", OpenAi)` 提前会让它误报。
            let expect = [
                ("claude", table::Vendor::Anthropic),
                ("gemini", table::Vendor::Google),
                ("deepseek", table::Vendor::DeepSeek),
                ("glm", table::Vendor::Zhipu),
                ("kimi", table::Vendor::Moonshot),
                ("moonshot", table::Vendor::Moonshot),
                ("qwen", table::Vendor::Alibaba),
                ("grok", table::Vendor::XAi),
                ("minimax", table::Vendor::MiniMax),
                ("hunyuan", table::Vendor::Tencent),
                ("sonar", table::Vendor::Perplexity),
                // ---- 以下是本轮补的（原先这一段整个缺失）----
                // 开放权重三条**必须在裸 gpt 之前**，理由见上。
                ("gpt-oss", table::Vendor::OpenWeight),
                ("llama", table::Vendor::OpenWeight),
                ("gemma", table::Vendor::OpenWeight),
                ("nemotron", table::Vendor::OpenWeight),
                ("codex", table::Vendor::OpenAi),
                ("gpt", table::Vendor::OpenAi),
                ("chat-latest", table::Vendor::OpenAi),
                ("o1", table::Vendor::OpenAi),
                ("o3", table::Vendor::OpenAi),
                ("o4", table::Vendor::OpenAi),
                ("mistral", table::Vendor::Mistral),
                ("ministral", table::Vendor::Mistral),
                ("codestral", table::Vendor::Mistral),
                ("command", table::Vendor::Cohere),
                ("seed-", table::Vendor::ByteDance),
                ("hy", table::Vendor::Tencent),
            ]
            .iter()
            .find(|(kw, _)| n.contains(kw))
            .map(|(_, v)| *v);
            if let Some(v) = expect {
                assert_eq!(
                    hit.vendor, v,
                    "模型 `{m}` 命中了片段 `{}`（厂商 {:?}），但它显然属于 {:?}",
                    hit.frag, hit.vendor, v
                );
            }
        }
    }

    /// 缓存价必须**非零**（除显式免费行）。
    ///
    /// 这条是为一个实测踩到的坑立的：单价原本用微美元存，`$3/MTok` → `3`，
    /// 缓存价 `3/10` 整数除法归零，结果缓存 token 完全不计费。纳美元下 `3000/10 = 300`。
    #[test]
    fn cache_prices_are_positive_unless_the_row_is_free() {
        for row in PRICING_TABLE {
            let p = ModelPricing::from_row(row);
            if row.input == 0.0 {
                assert_eq!(p.cache_read_per_ntx, 0, "{} 免费行四项应全 0", row.frag);
                continue;
            }
            assert!(p.input_per_ntx > 0, "{} 输入价为 0", row.frag);
            assert!(p.output_per_ntx > 0, "{} 输出价为 0", row.frag);
            assert!(
                p.cache_read_per_ntx > 0,
                "{} 缓存命中价为 0 —— 会让这类 token 静默免费",
                row.frag
            );
            assert!(p.cache_creation_per_ntx > 0, "{} 缓存创建价为 0", row.frag);
        }
    }

    /// 显式 `cache_read` 必须**覆盖** 0.1× 默认，否则整类厂商的缓存成本会偏 2~5 倍。
    ///
    /// 故障注入判据：让 `from_row` 忽略 `row.cache_read` → 必须变红。
    #[test]
    fn explicit_cache_read_overrides_the_tenth_rule() {
        // DeepSeek V4 Pro：input 1.32 → 0.1× 会算成 132 n$，实际是 44。
        let p = lookup_pricing("deepseek-v4-pro").unwrap();
        assert_eq!(p.input_per_ntx, 1320);
        assert_eq!(p.cache_read_per_ntx, 44, "DeepSeek 的缓存命中约 0.033×，不是 0.1×");
        // gpt-4o：0.5×（250 → 1250）
        let p = lookup_pricing("gpt-4o").unwrap();
        assert_eq!(p.cache_read_per_ntx, 1250, "gpt-4o 的缓存命中是 0.5×");
        // Anthropic 仍是 0.1×（表里显式给的值与默认一致）
        let p = lookup_pricing("claude-opus-5").unwrap();
        assert_eq!(p.input_per_ntx, 5000);
        assert_eq!(p.cache_read_per_ntx, 500);
        assert_eq!(p.cache_creation_per_ntx, 6250, "Anthropic 5m 写入是 1.25×");
    }

    /// 三条**数值回归**，各锁住一个真实存在过的偏差方向。
    #[test]
    fn known_mispricings_stay_fixed() {
        let one_m = TokenUsage { input: 1_000_000, output: 0, cache_read: 0, cache_creation: 0 };

        // ① 3× 高估：现役 Opus 是 $5/MTok，旧表按退役价 $15 算。
        let (c, src) = estimate_cost(&one_m, Some("claude-opus-5"), None);
        assert_eq!(c, Some(5_000_000_000), "claude-opus-5 = $5/MTok");
        assert_eq!(src, PricingSource::Exact);
        // 用户机器上最常见的那个名字（带 -thinking 后缀）必须走同一档。
        assert_eq!(estimate_cost(&one_m, Some("claude-opus-5-thinking"), None).0, Some(5_000_000_000));
        assert_eq!(estimate_cost(&one_m, Some("claude-opus-4-8"), None).0, Some(5_000_000_000));
        // 退役型号仍按它自己的价（历史用量要算对）
        assert_eq!(estimate_cost(&one_m, Some("claude-opus-4-1"), None).0, Some(15_000_000_000));

        // ② 3.3× 低估：Fable 5 是 $10/MTok，旧表按裸 `claude` 的 $3 算。
        assert_eq!(estimate_cost(&one_m, Some("claude-fable-5"), None).0, Some(10_000_000_000));

        // ③ 100× 高估：gpt-4.1-nano 是 $0.10/MTok，旧表按 `gpt-4` 的 $10 算。
        assert_eq!(estimate_cost(&one_m, Some("gpt-4.1-nano"), None).0, Some(100_000_000));
        // 归一是这条的前提：`gpt-4.1-nano` 里的 `.` 不拍平就命中不了 `gpt-4-1-nano`。
        assert_eq!(estimate_cost(&one_m, Some("gpt-4_1_nano"), None).0, Some(100_000_000));

        // ④ 12.5× 高估：gemini flash-lite 是 $0.10/MTok，旧表按裸 `gemini` 的 $1.25 算。
        assert_eq!(estimate_cost(&one_m, Some("gemini-2.5-flash-lite"), None).0, Some(100_000_000));

        // ⑤ 整族 Unknown：Codex 用的那些名字必须有价。
        assert_eq!(estimate_cost(&one_m, Some("gpt-5.6-sol"), None).0, Some(4_000_000_000));
        assert_eq!(estimate_cost(&one_m, Some("gpt-5.4-mini"), None).0, Some(750_000_000));

        // ⑥ **13.6× 高估**：`o1-mini` 缺行 → 按最长片段落到 `o1`（$15/$60）。
        //    与 ③ 的 `gpt-4.1-nano → gpt-4` 同一机制：短片段是长名字的前缀，缺一行就继承贵档。
        assert_eq!(
            estimate_cost(&one_m, Some("o1-mini"), None).0,
            Some(1_100_000_000),
            "o1-mini = $1.10/MTok，不是 o1 的 $15"
        );

        // ⑦ **未来小版本不许钉在最老的兄弟行上**（`fragment_matches` 的版本边界判据）。
        //    这三条此前都是 `Exact` → 界面不打 `≈`，用户看到一个自信的错数字。
        //    偏低尤其糟：它让人不知不觉超支（模块头写明「宁可偏高」）。
        let flagship_gpt = estimate_cost(&one_m, Some("gpt-5.6-sol"), None).0;
        for future in ["gpt-5.6", "gpt-5.7", "gpt-5.9"] {
            assert_eq!(
                estimate_cost(&one_m, Some(future), None).0,
                flagship_gpt,
                "{future} 应落到 gpt 家族兜底（= 现役旗舰价），而不是 gpt-5 那行的 $1.25"
            );
        }
        let flagship_glm = estimate_cost(&one_m, Some("glm-5.3"), None).0;
        assert_eq!(
            estimate_cost(&one_m, Some("glm-5.9"), None).0,
            flagship_glm,
            "glm-5.9 应落到 glm 家族兜底，而不是 glm-5 那行"
        );
        // 而**现役的裸代号本身**必须仍按它自己那行算（这条防止上面的修法过头）
        assert_eq!(
            estimate_cost(&one_m, Some("gpt-5"), None).0,
            Some(1_250_000_000),
            "gpt-5 自己仍在役，必须按 $1.25 算"
        );
        // 退役行不得成为未来小版本的事实兜底：`gpt-4.5` 曾落到 live:false 的 gpt-4（$30）
        assert_ne!(
            estimate_cost(&one_m, Some("gpt-4.5"), None).0,
            Some(30_000_000_000),
            "gpt-4.5 不该继承已退役的 gpt-4 行"
        );
    }

    /// `Exact` 与 `Family` 必须都是**可达**的档位。
    ///
    /// 旧实现里 `Exact` 是死路径（内置表只有 9 条全等匹配的退役名，现实中一条都不命中），
    /// 于是界面上每一行都带「≈」，两个视觉档位退化成一个。
    #[test]
    fn both_exact_and_family_are_reachable() {
        let u = TokenUsage { input: 1000, output: 0, cache_read: 0, cache_creation: 0 };
        // 具体型号 → Exact
        assert_eq!(estimate_cost(&u, Some("claude-opus-5"), None).1, PricingSource::Exact);
        assert_eq!(estimate_cost(&u, Some("gpt-5.6-sol"), None).1, PricingSource::Exact);
        // 只命中家族兜底 → Family（一个尚不存在的新世代）
        assert_eq!(estimate_cost(&u, Some("claude-opus-9-9"), None).1, PricingSource::Family);
        assert_eq!(estimate_cost(&u, Some("gpt-9-turbo"), None).1, PricingSource::Family);
        // 完全不认识 → Unknown，且**不猜**
        assert_eq!(estimate_cost(&u, Some("totally-unknown-llm"), None).1, PricingSource::Unknown);
        assert_eq!(estimate_cost(&u, Some("ernie-5-turbo"), None).1, PricingSource::Unknown);
    }

    /// 免费模型如实显示 $0，而不是被家族兜底开出一张 $1.4/MTok 的账单。
    #[test]
    fn free_models_are_priced_at_zero_not_by_family_fallback() {
        let u = TokenUsage { input: 1_000_000, output: 1_000_000, cache_read: 0, cache_creation: 0 };
        assert_eq!(estimate_cost(&u, Some("glm-4.7-flash"), None).0, Some(0));
        // 而同族的收费型号照常收费（防「免费判据吃掉整个家族」）
        assert!(estimate_cost(&u, Some("glm-4.7"), None).0.unwrap() > 0);
        assert!(estimate_cost(&u, Some("glm-4.7-flashx"), None).0.unwrap() > 0);
    }

    #[test]
    fn format_usd_displays_four_decimals() {
        assert_eq!(format_usd_from_nano(0), "$0.0000");
        assert_eq!(format_usd_from_nano(65_250_000), "$0.0653");
        assert_eq!(format_usd_from_nano(1_000_000_000), "$1.0000");
        assert_eq!(format_usd_from_nano(12_345_678_900), "$12.3457");
    }

    #[test]
    fn calculate_cost_with_multiplier() {
        let p = ModelPricing::from_official(3.0, 15.0);
        let usage = TokenUsage { input: 10_000, output: 2_000, cache_read: 5_000, cache_creation: 1_000 };
        // 10k×3000 + 2k×15000 + 5k×300 + 1k×3750 = 65_250_000 n$ = $0.06525
        assert_eq!(p.calculate_cost_nano(&usage, 1.0), 65_250_000);
        assert_eq!(p.calculate_cost_nano(&usage, 0.3), 19_575_000);
        let no_cache = TokenUsage { cache_read: 0, cache_creation: 0, ..usage };
        assert!(
            p.calculate_cost_nano(&no_cache, 1.0) < 65_250_000,
            "缓存 token 必须计入成本，否则计费偏低"
        );
    }

    /// 倍率的各种笔误都必须退回 1.0，绝不算成 0 或天文数字。
    #[test]
    fn estimate_cost_survives_bad_multiplier() {
        let usage = TokenUsage { input: 1_000_000, output: 0, cache_read: 0, cache_creation: 0 };
        let base = Some(5_000_000_000); // claude-opus-5 $5/MTok

        assert_eq!(estimate_cost(&usage, Some("claude-opus-5"), Some("0.3")).0, Some(1_500_000_000));
        assert_eq!(estimate_cost(&usage, Some("claude-opus-5"), Some("abc")).0, base, "非法倍率退回 1.0");
        assert_eq!(estimate_cost(&usage, Some("claude-opus-5"), Some("0")).0, base, "倍率 0 视为未填");

        // `"inf"` / `"1e400"` 都能成功解析成 f64::INFINITY，且 `INFINITY > 0.0` 为真 ——
        // 它们此前能穿过那道门，乘出来的 f64 转 u64 是饱和转换，金额变成 u64::MAX。
        for absurd in ["inf", "Infinity", "1e400", "-inf", "NaN", "nan"] {
            assert_eq!(
                estimate_cost(&usage, Some("claude-opus-5"), Some(absurd)).0,
                base,
                "倍率「{absurd}」不是有限正数，必须退回 1.0"
            );
        }
        let over = format!("{}", MAX_COST_MULTIPLIER + 1.0);
        assert_eq!(estimate_cost(&usage, Some("claude-opus-5"), Some(&over)).0, base, "超上界退回 1.0");
        // 边界本身仍然生效（判据是 <=）
        let at = format!("{MAX_COST_MULTIPLIER}");
        assert_eq!(
            estimate_cost(&usage, Some("claude-opus-5"), Some(&at)).0,
            Some(5_000_000_000_000)
        );
    }

    /// 无模型名 / 空白名一律 Unknown，不估成 0。
    #[test]
    fn no_model_name_means_unknown_not_zero() {
        let usage = TokenUsage { input: 1_000_000, output: 0, cache_read: 0, cache_creation: 0 };
        for hint in [None, Some(""), Some("   ")] {
            let (cost, src) = estimate_cost(&usage, hint, None);
            assert_eq!(cost, None, "hint={hint:?} 应无价");
            assert_eq!(src, PricingSource::Unknown);
        }
    }

    /// 价格核对日期必须是个能解析的日期，且不能是空串
    /// （界面要显示它，空串会变成一句「核对日期：」的残句）。
    #[test]
    fn verified_on_is_a_real_date() {
        assert!(
            chrono::NaiveDate::parse_from_str(PRICE_TABLE_VERIFIED_ON, "%Y-%m-%d").is_ok(),
            "PRICE_TABLE_VERIFIED_ON 必须是 YYYY-MM-DD：{PRICE_TABLE_VERIFIED_ON}"
        );
    }
}

