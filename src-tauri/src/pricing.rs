//! 模型单价表与成本核算。
//!
//! 关键设计（对齐 cc-switch 的 `model_pricing` 表）：
//! - 成本全部用**定点数运算**（单位：百万分之一美元，6 位精度）避免浮点累加误差。
//! - 内置单价表覆盖 Anthropic 官方模型（2026-08 价格表，定期更新）。
//! - 用户可为中转商 Key 配置**倍率**（如 `0.3` 表示三折），用量日志按实际 Key 乘倍率计费。
//! - 缓存创建单价 = 输入单价 × 1.25（Anthropic 官网定价）。
//! - 缓存命中单价 = 输入单价 ÷ 10。
//!
//! Token → 美元换算链路：
//! ```text
//! TokenUsage {input, output, cache_read, cache_creation}
//!   ↓ 查单价表 ModelPricing {input_mtx, output_mtx, ..}
//!   ↓ 各乘对应 token 数
//!   ↓ 四项求和 → 微美元（µUSD，百万分之一）
//!   ↓ 乘 Key 倍率 → 最终成本
//!   ↓ format_usd_from_micro() → "$0.012345"（显示用）
//! ```

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// 单个模型的单价结构（单位：**纳美元 / token**，即 1e-9 USD）。
///
/// 举例：`input_per_ntx: 3000` = 每 token 3000 n$ = $0.000003（即每百万 token $3）。
///
/// **为什么用纳美元而不是微美元**：微美元精度下 `$3/MTok` 只能存成 `3`，
/// 而缓存命中价 = 输入价 ÷ 10 → `3/10` 整数除法直接归零，等于缓存不计费。
/// 纳美元下是 `3000/10 = 300`，精度足够。这是实测（`cache_read_per_mtx == 0`）
/// 才发现的：一个纯粹的单位选择失误会让整类 token 静默免费。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// 输入单价（nano-USD per token）
    pub input_per_ntx: u64,
    /// 输出单价
    pub output_per_ntx: u64,
    /// 缓存命中单价（通常 = input ÷ 10）
    pub cache_read_per_ntx: u64,
    /// 缓存创建单价（通常 = input × 1.25）
    pub cache_creation_per_ntx: u64,
}

impl ModelPricing {
    /// 从官方单价（$/MTok）构造，自动算缓存价。
    ///
    /// $X/MTok = X ÷ 1e6 USD/token = X × 1000 nano-USD/token。
    pub fn from_official(input_per_mtok: f64, output_per_mtok: f64) -> Self {
        let input = (input_per_mtok * 1000.0).round() as u64;
        let output = (output_per_mtok * 1000.0).round() as u64;
        Self {
            input_per_ntx: input,
            output_per_ntx: output,
            cache_read_per_ntx: input / 10,            // 官方：缓存命中价 = 输入价 ÷ 10
            cache_creation_per_ntx: input + input / 4, // 1.25 倍，避免浮点：x + x/4
        }
    }

    /// 计算一次请求的成本（**纳美元**），含 Key 倍率（如 0.3 表示三折）。
    ///
    /// 用 u128 中间累加：单次请求 token 数 × 单价在 u64 下有溢出余量，但按日/按月
    /// 汇总时会累加成千上万次，u128 给足空间。
    pub fn calculate_cost_nano(
        &self,
        usage: &crate::model::TokenUsage,
        multiplier: f64,
    ) -> u64 {
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
/// **当前只被测试用到**：金额是在前端渲染的（`UsagePage.tsx` / `FloatingWidget.tsx`
/// 各有一份同口径的 `fmtUsd`），IPC 传的是原始 `costNano` 整数 —— 传数值而非
/// 预格式化字符串，前端才能自己决定精度、做小计与排序。
///
/// 保留它有两个用途：① 钉住「4 位小数 + 四舍五入」这个显示口径，前端那两份实现
/// 要与它对齐（测试是唯一的对照物）；② 后端日后要在日志/诊断报告里打印金额时直接可用。
#[cfg_attr(not(test), allow(dead_code))]
pub fn format_usd_from_nano(nano: u64) -> String {
    // 纳美元 → 万分之一美元（4 位小数）：÷ 1e5，四舍五入
    let ten_thousandths = (nano + 50_000) / 100_000;
    format!("${}.{:04}", ten_thousandths / 10_000, ten_thousandths % 10_000)
}

/// 内置单价表（Anthropic 官方模型，2026-08 价格，单位：$/MTok）。
///
/// 来源：https://docs.anthropic.com/en/docs/about-claude/models#model-comparison-table
/// 更新频率：每季度检查一次官网价格表，新模型发布时补充。
fn builtin_pricing() -> &'static std::collections::HashMap<&'static str, ModelPricing> {
    static PRICING: OnceLock<std::collections::HashMap<&'static str, ModelPricing>> = OnceLock::new();
    PRICING.get_or_init(|| {
        let mut m = std::collections::HashMap::new();

        // Opus 系列（最贵、最强）
        m.insert("claude-opus-4-20250514", ModelPricing::from_official(15.0, 75.0));
        m.insert("claude-opus-4", ModelPricing::from_official(15.0, 75.0));

        // Sonnet 系列（主力）
        m.insert("claude-sonnet-4-20250514", ModelPricing::from_official(3.0, 15.0));
        m.insert("claude-sonnet-4", ModelPricing::from_official(3.0, 15.0));
        m.insert("claude-3-5-sonnet-20241022", ModelPricing::from_official(3.0, 15.0));
        m.insert("claude-3-5-sonnet-20240620", ModelPricing::from_official(3.0, 15.0));
        m.insert("claude-3-sonnet-20240229", ModelPricing::from_official(3.0, 15.0));

        // Haiku 系列（最便宜）
        m.insert("claude-3-5-haiku-20241022", ModelPricing::from_official(0.8, 4.0));
        m.insert("claude-3-haiku-20240307", ModelPricing::from_official(0.25, 1.25));

        m
    })
}

/// 家族级兜底单价：精确名查不到时按**名字里的家族关键词**匹配。
///
/// 为什么需要：内置表只可能覆盖官方模型名，而本应用的主场景是中转站 ——
/// 用户那边的模型名可能是 `claude-opus-4-8`、`glm-4.6`、`deepseek-v4-pro`
/// 这类表里没有的。没有兜底时这些 Key 一律算不出金额，用量页会大面积显示「—」，
/// 而用户配了倍率却看不到钱，会以为功能坏了。
///
/// **顺序敏感**：`opus` 必须排在 `claude` 之前 —— `claude-opus-4-8` 同时含两个词，
/// 先匹配到 `claude` 就会按 Sonnet 价算，把最贵的模型算成中档价。
const FAMILY_FALLBACK: &[(&str, f64, f64)] = &[
    // (家族关键词, 输入 $/MTok, 输出 $/MTok)
    ("opus", 15.0, 75.0),
    ("sonnet", 3.0, 15.0),
    ("haiku", 0.8, 4.0),
    // 常见中转模型（取各家官网公开价的量级；用户可用倍率校准）
    ("deepseek", 0.27, 1.1),
    ("glm", 0.6, 2.2),
    ("qwen", 0.4, 1.2),
    ("kimi", 0.6, 2.5),
    ("gemini", 1.25, 5.0),
    ("gpt-4o", 2.5, 10.0),
    ("gpt-4", 10.0, 30.0),
    ("grok", 3.0, 15.0),
    // 兜底的兜底：名字里有 claude 但没命中上面任何档，按 Sonnet 估
    ("claude", 3.0, 15.0),
];

/// 按家族关键词兜底查单价。返回 `(单价, 命中的关键词)`。
fn family_fallback(model_name: &str) -> Option<(ModelPricing, &'static str)> {
    let lower = model_name.trim().to_ascii_lowercase();
    FAMILY_FALLBACK.iter().find_map(|(kw, i, o)| {
        lower
            .contains(kw)
            .then(|| (ModelPricing::from_official(*i, *o), *kw))
    })
}

/// 按模型名查单价，支持「对外映射名 → 真实名」回退。
///
/// 举例：用户配了映射 `gpt-4-turbo -> claude-sonnet-4`，日志里记的是 `gpt-4-turbo`，
/// 这里先查 `gpt-4-turbo`（查不到），再查 `claude-sonnet-4`（能查到）。
pub fn lookup_pricing(model_name: &str, real_model_name: Option<&str>) -> Option<&'static ModelPricing> {
    let pricing = builtin_pricing();
    pricing
        .get(model_name)
        .or_else(|| real_model_name.and_then(|r| pricing.get(r)))
}

/// 单价的来源，供界面如实标注精度。
///
/// 用户会拿这个面板的数字与中转站账单对比，所以**必须让他知道这数是怎么来的**：
/// 精确命中官方价、还是按家族名猜的、还是压根没有单价。含糊其辞会让用户
/// 把估算当账单，对不上时以为程序算错了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PricingSource {
    /// 精确命中内置官方单价表
    Exact,
    /// 按家族关键词兜底（如 `claude-opus-4-8` → opus 档）
    Family,
    /// 没有任何单价可用 → 金额不可估算
    Unknown,
}

/// 估算一段用量的成本（纳美元）+ 单价来源。
///
/// `model_hint` 传该 Key 的代表模型名（默认兜底模型或首个模型）。
/// **本函数刻意不按模型逐条算**：用量累加器的键是 `(分类, keyId)`、不含模型名，
/// 要按模型精确计费得把落盘格式再升一版。而用户选定的方案是「每个 Key 一个折扣倍率」，
/// 倍率本就挂在 Key 上，故按 Key 估算与该方案一致。代价是同一 Key 跑多个不同档位
/// 模型时会有偏差 —— 界面必须标明这是估算（见 `usage.estimateHint`）。
pub fn estimate_cost(
    usage: &crate::model::TokenUsage,
    model_hint: Option<&str>,
    multiplier: Option<&str>,
) -> (Option<u64>, PricingSource) {
    // 倍率解析失败（用户填了 "abc"）时按 1.0，不让一个笔误把金额算成 0。
    let mult = multiplier
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|m| *m > 0.0)
        .unwrap_or(1.0);

    let Some(name) = model_hint.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, PricingSource::Unknown);
    };

    if let Some(p) = lookup_pricing(name, None) {
        return (Some(p.calculate_cost_nano(usage, mult)), PricingSource::Exact);
    }
    if let Some((p, _kw)) = family_fallback(name) {
        return (Some(p.calculate_cost_nano(usage, mult)), PricingSource::Family);
    }
    (None, PricingSource::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缓存价必须**非零**。这条测试是为一个实测踩到的坑立的：
    /// 单价原本用微美元存，`$3/MTok` → `3`，缓存价 `3/10` 整数除法归零，
    /// 结果缓存 token 完全不计费。纳美元下 `3000/10 = 300`，精度够。
    #[test]
    fn pricing_from_official_calculates_cache_prices() {
        // Sonnet 官方价：$3/MTok 输入、$15/MTok 输出
        let p = ModelPricing::from_official(3.0, 15.0);
        assert_eq!(p.input_per_ntx, 3000); // n$/token
        assert_eq!(p.output_per_ntx, 15_000);
        assert_eq!(p.cache_read_per_ntx, 300, "缓存命中价 = 输入价 ÷ 10，绝不能是 0");
        assert_eq!(p.cache_creation_per_ntx, 3750, "缓存创建价 = 输入价 × 1.25");

        // 连最便宜的 Haiku（$0.25/MTok）缓存价也不能归零
        let haiku = ModelPricing::from_official(0.25, 1.25);
        assert_eq!(haiku.input_per_ntx, 250);
        assert_eq!(haiku.cache_read_per_ntx, 25, "最低价模型的缓存价也必须 > 0");
    }

    #[test]
    fn calculate_cost_with_multiplier() {
        let p = ModelPricing::from_official(3.0, 15.0);
        let usage = crate::model::TokenUsage {
            input: 10_000,
            output: 2_000,
            cache_read: 5_000,
            cache_creation: 1_000,
        };
        // 10k×3000 + 2k×15000 + 5k×300 + 1k×3750
        // = 30_000_000 + 30_000_000 + 1_500_000 + 3_750_000 = 65_250_000 n$ = $0.06525
        let base = p.calculate_cost_nano(&usage, 1.0);
        assert_eq!(base, 65_250_000);

        // 中转商三折
        let discounted = p.calculate_cost_nano(&usage, 0.3);
        assert_eq!(discounted, 19_575_000);

        // 缓存 token 确实计入了成本（把它们清零，成本必须变小）
        let no_cache = crate::model::TokenUsage { cache_read: 0, cache_creation: 0, ..usage };
        assert!(
            p.calculate_cost_nano(&no_cache, 1.0) < base,
            "缓存 token 必须计入成本，否则计费偏低"
        );
    }

    #[test]
    fn format_usd_displays_four_decimals() {
        assert_eq!(format_usd_from_nano(0), "$0.0000");
        assert_eq!(format_usd_from_nano(65_250_000), "$0.0653"); // 四舍五入
        assert_eq!(format_usd_from_nano(1_000_000_000), "$1.0000");
        assert_eq!(format_usd_from_nano(12_345_678_900), "$12.3457");
    }

    #[test]
    fn builtin_pricing_covers_current_models() {
        let pricing = builtin_pricing();
        // Opus
        assert!(pricing.contains_key("claude-opus-4"));
        // Sonnet（主力）
        assert!(pricing.contains_key("claude-sonnet-4"));
        assert!(pricing.contains_key("claude-3-5-sonnet-20241022"));
        // Haiku
        assert!(pricing.contains_key("claude-3-5-haiku-20241022"));

        // 价格检查：Sonnet 应该是 $3/$15 per MTok → 3000/15000 n$ per token
        let sonnet = pricing.get("claude-sonnet-4").unwrap();
        assert_eq!(sonnet.input_per_ntx, 3000);
        assert_eq!(sonnet.output_per_ntx, 15_000);

        // 全表自检：任何模型的缓存价都不能是 0（那等于该类 token 免费）
        for (name, p) in pricing.iter() {
            assert!(p.input_per_ntx > 0, "{name} 输入价为 0");
            assert!(p.output_per_ntx > 0, "{name} 输出价为 0");
            assert!(p.cache_read_per_ntx > 0, "{name} 缓存命中价为 0，会导致缓存 token 免费");
            assert!(p.cache_creation_per_ntx > 0, "{name} 缓存创建价为 0");
        }
    }

    /// 家族兜底的**顺序**是有讲究的：`claude-opus-4-8` 同时含 "opus" 与 "claude"，
    /// 必须命中 opus 档（$15/$75）。若 "claude" 排在前面，最贵的模型会被按
    /// 中档 Sonnet 价计算 —— 金额偏低到只有 1/5，而界面上完全看不出错。
    #[test]
    fn family_fallback_prefers_specific_tier_over_generic_claude() {
        let (p, kw) = family_fallback("claude-opus-4-8").expect("应命中家族兜底");
        assert_eq!(kw, "opus", "含 opus 的名字必须命中 opus 档，而非泛化的 claude");
        assert_eq!(p.input_per_ntx, 15_000);
        assert_eq!(p.output_per_ntx, 75_000);

        // 中转站常见的非官方名也要能兜住
        assert_eq!(family_fallback("glm-4.6").unwrap().1, "glm");
        assert_eq!(family_fallback("deepseek-v4-pro").unwrap().1, "deepseek");
        // 泛 claude 名（无档位词）落到 Sonnet 档
        assert_eq!(family_fallback("claude-custom-x").unwrap().1, "claude");
        // 完全不认识的名字：不猜
        assert!(family_fallback("totally-unknown-llm").is_none());
    }

    /// 估算成本要如实报告单价来源，且倍率笔误不能把金额算成 0。
    #[test]
    fn estimate_cost_reports_source_and_survives_bad_multiplier() {
        let usage = crate::model::TokenUsage {
            input: 1_000_000,
            output: 0,
            cache_read: 0,
            cache_creation: 0,
        };

        // 精确命中
        let (cost, src) = estimate_cost(&usage, Some("claude-sonnet-4"), None);
        assert_eq!(src, PricingSource::Exact);
        assert_eq!(cost, Some(3_000_000_000), "100 万 token × $3/MTok = $3");

        // 家族兜底
        let (_, src) = estimate_cost(&usage, Some("claude-opus-4-8"), None);
        assert_eq!(src, PricingSource::Family);

        // 无模型名 / 不认识 → 不估算（而不是估成 0）
        assert_eq!(estimate_cost(&usage, None, None).1, PricingSource::Unknown);
        assert_eq!(estimate_cost(&usage, None, None).0, None);
        assert_eq!(estimate_cost(&usage, Some("zzz"), None).0, None);

        // 倍率生效
        let (discounted, _) = estimate_cost(&usage, Some("claude-sonnet-4"), Some("0.3"));
        assert_eq!(discounted, Some(900_000_000));

        // 倍率笔误：按 1.0 处理，不能算成 0（那会让整页金额凭空消失）
        let (fallback, _) = estimate_cost(&usage, Some("claude-sonnet-4"), Some("abc"));
        assert_eq!(fallback, Some(3_000_000_000), "非法倍率应退回 1.0");
        let (zero_mult, _) = estimate_cost(&usage, Some("claude-sonnet-4"), Some("0"));
        assert_eq!(zero_mult, Some(3_000_000_000), "倍率 0 视为未填，退回 1.0");
    }

    #[test]
    fn lookup_pricing_falls_back_to_real_name() {
        // 直接查到
        assert!(lookup_pricing("claude-sonnet-4", None).is_some());
        // 查不到但 real_model 能查到
        assert!(lookup_pricing("gpt-4-turbo", Some("claude-sonnet-4")).is_some());
        // 都查不到
        assert!(lookup_pricing("unknown-model", Some("also-unknown")).is_none());
    }
}
