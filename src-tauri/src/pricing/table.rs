//! 模型单价**数据表**。只放数据，不放逻辑（逻辑在 [`super`]）。
//!
//! # 匹配语义（与旧实现的关键差别）
//!
//! 旧实现是「表序 + 裸 `contains`」：`FAMILY_FALLBACK.iter().find(|kw| lower.contains(kw))`。
//! 那让**表的顺序成了语义的一部分** —— 排错了不报错，金额只是悄悄变大或变小。
//! 现在是「**归一 + 最长片段命中**」：先把 `.`/`/`/`_` 拍成 `-` 再小写，然后在所有
//! `contains` 命中里取**片段最长**的那一行。表序不再有意义，
//! `pricing_is_independent_of_table_order` 那条测试直接把这件事钉住。
//!
//! # 家族兜底行 = 该家族**当前旗舰**的价
//!
//! 兜底行（裸家族名，如 `claude-opus` / `gpt` / `glm`）只会被「没命中任何具体行」的名字命中，
//! 也就是**将来的新版本**或中转站的私有别名。它的值由 [`FAMILY_FLAGSHIP`] 显式指向某一具体行，
//! 由 `family_default_equals_designated_flagship` 机械校验 —— 靠注释提醒靠不住，
//! 这张表上一版就是这么错的：`("opus", 15.0, 75.0)` 是**已退役的 Opus 4/4.1** 的价，
//! 而现役 Opus 4.5~5 全是 $5/$25，于是用户面板上每一笔 Opus 花费都是真值的 **3 倍**。
//!
//! # 认不出的新版本一律**偏高**
//!
//! 这是个帮人控预算的面板。低估让人不知不觉超支（无声），高估只会让人多看两眼
//! （同样无声，但代价小得多）。两种都无声，故选代价小的那种。
//! 分档计价的模型（Gemini Pro 的 200k 档、Grok 的 200k 档、DeepSeek 的峰谷时段）一律取**贵档**。
//!
//! # 取证
//!
//! 价格核对日期见 [`PRICE_TABLE_VERIFIED_ON`]（界面 tooltip 会显示它，让用户自己判断陈旧度）。
//! 各家来源逐条记在下面的分区注释里。**拿不到权威价的一律不给行**（落 Unknown、界面显示「—」
//! 并点出模型名），绝不编一个数 —— 编出来的数用户会当账单看。

/// 价格核对日期。界面显示它，让用户能判断这张表有多旧。
///
/// 刻意**不做**「生效日期」列（某些厂商已公告未来涨价）：时间炸弹式的逻辑难测，
/// 而一个显式的核对日期 + 用户自己填的计费倍率已经够用。
pub(crate) const PRICE_TABLE_VERIFIED_ON: &str = "2026-08-25";

/// 声明厂商。**只用于不变量校验**（`no_fragment_matches_another_vendors_model`，
/// 防「短片段乱吃别家模型名」），不参与查价 —— 故生产代码里读不到它。
///
/// 留着它的理由是那条测试：没有厂商归属，「`sonar` 这一行会不会吃掉 `claude-sonnet`」
/// 这类问题就只能靠人眼看，而看漏的表现是某个厂商的模型全按另一家的价算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 见上：仅供 #[cfg(test)] 的不变量校验读取
pub(crate) enum Vendor {
    Anthropic,
    OpenAi,
    Google,
    DeepSeek,
    Zhipu,
    Moonshot,
    Alibaba,
    XAi,
    Mistral,
    MiniMax,
    Tencent,
    ByteDance,
    Cohere,
    Perplexity,
    OpenWeight,
}

/// 一行单价（单位：$/MTok）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row {
    /// **归一后**的模型名片段（小写、`-` 分隔）。最长命中优先。
    pub(crate) frag: &'static str,
    /// 声明厂商。仅供不变量校验读取，见 [`Vendor`]。
    #[allow(dead_code)]
    pub(crate) vendor: Vendor,
    pub(crate) input: f64,
    pub(crate) output: f64,
    /// 缓存命中价。`None` = 按 `input × 0.1`。
    ///
    /// 为什么要能显式给：0.1× 只对 Anthropic / gpt-5 族 / Gemini 成立。实测比例差很多 ——
    /// DeepSeek 约 0.033×（写死 0.1 会高估 3 倍）、gpt-4o 与 o1 是 0.5×、
    /// o3/o4-mini/gpt-4.1 系是 0.25×、GLM 约 0.19×（这些写死 0.1 会低估 2~5 倍）。
    /// 而缓存 token 常常是 input 的好几倍，这个系数错了金额就整体偏。
    pub(crate) cache_read: Option<f64>,
    /// 缓存**写入**价。`None` = 按 `input × 1.25`（Anthropic 的 5m 写入比例）。
    ///
    /// ⚠️ 目前只对 Anthropic 有实际意义：`upstream/usage.rs` 只从 Anthropic 的
    /// `cache_creation_input_tokens` 取这个数，非 Anthropic 上游恒为 0，所以这一列
    /// 乘什么都不影响结果。**谁将来给 OpenAI 侧补上那个字段，必须先回来把这一列填全** ——
    /// 否则会静默按 1.25× 多收。
    pub(crate) cache_write: Option<f64>,
    /// 现役 = true。退役型号留在表里（历史用量要算对），但**不许**被旗舰指针指向。
    ///
    /// 仅供 `flagship_row_must_be_live` 校验读取 —— 那条测试防的是「兜底价停在退役价上」，
    /// 也就是本表上一版 3× 高估的根因。
    #[allow(dead_code)]
    pub(crate) live: bool,
}

const fn r(frag: &'static str, vendor: Vendor, input: f64, output: f64) -> Row {
    Row { frag, vendor, input, output, cache_read: None, cache_write: None, live: true }
}
/// 带显式缓存命中价。
const fn rc(frag: &'static str, vendor: Vendor, input: f64, output: f64, cr: f64) -> Row {
    Row { frag, vendor, input, output, cache_read: Some(cr), cache_write: None, live: true }
}
/// 带显式缓存命中价 + 写入价。
const fn rcw(
    frag: &'static str,
    vendor: Vendor,
    input: f64,
    output: f64,
    cr: f64,
    cw: f64,
) -> Row {
    Row { frag, vendor, input, output, cache_read: Some(cr), cache_write: Some(cw), live: true }
}
/// 标记为已退役（价格仍需保留：历史用量要算对）。
const fn retired(row: Row) -> Row {
    Row { live: false, ..row }
}

/// 单价表。**顺序不影响语义**（最长片段命中），按厂商分组只为可读。
pub(crate) const PRICING_TABLE: &[Row] = &[
    // ======================= Anthropic =======================
    // 来源：https://platform.claude.com/docs/en/about-claude/pricing（2026-08-25 实测）
    // 该页明示缓存乘数：5m 写 1.25×、1h 写 2×、读 0.1×。本表用 5m 写。
    rcw("claude-fable-5", Vendor::Anthropic, 10.0, 50.0, 1.00, 12.50),
    rcw("claude-mythos-5", Vendor::Anthropic, 10.0, 50.0, 1.00, 12.50),
    rcw("claude-opus-5", Vendor::Anthropic, 5.0, 25.0, 0.50, 6.25),
    rcw("claude-opus-4-8", Vendor::Anthropic, 5.0, 25.0, 0.50, 6.25),
    rcw("claude-opus-4-7", Vendor::Anthropic, 5.0, 25.0, 0.50, 6.25),
    rcw("claude-opus-4-6", Vendor::Anthropic, 5.0, 25.0, 0.50, 6.25),
    rcw("claude-opus-4-5", Vendor::Anthropic, 5.0, 25.0, 0.50, 6.25),
    // 官方页标注 retired：只有这两代才是 $15/$75。旧表把整个 opus 家族都按它们算。
    retired(rcw("claude-opus-4-1", Vendor::Anthropic, 15.0, 75.0, 1.50, 18.75)),
    retired(rcw("claude-opus-4", Vendor::Anthropic, 15.0, 75.0, 1.50, 18.75)),
    // Sonnet 5 的 $2/$10：官方页有一条 note 明说原定 2026-09-01 涨到 $3/$15 的计划**取消**了，
    // 现在 $2/$10 就是标准价。故这里不留「将来涨价」的伏笔。
    rcw("claude-sonnet-5", Vendor::Anthropic, 2.0, 10.0, 0.20, 2.50),
    rcw("claude-sonnet-4-6", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75),
    rcw("claude-sonnet-4-5", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75),
    retired(rcw("claude-sonnet-4", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75)),
    rcw("claude-haiku-4-5", Vendor::Anthropic, 1.0, 5.0, 0.10, 1.25),
    retired(rcw("claude-3-5-haiku", Vendor::Anthropic, 0.80, 4.00, 0.08, 1.00)),
    retired(rcw("claude-3-haiku", Vendor::Anthropic, 0.25, 1.25, 0.025, 0.3125)),
    // 3.7 / 3.5 / 3 Sonnet 已不在官方价表上（retired 且未列价）。沿用历史 $3/$15 供历史用量对账，
    // 标 live=false，且不许被旗舰指针指向。
    retired(rcw("claude-3-7-sonnet", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75)),
    retired(rcw("claude-3-5-sonnet", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75)),
    retired(rcw("claude-3-sonnet", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75)),
    // 家族兜底（= 各档现役旗舰，见 FAMILY_FLAGSHIP）
    rcw("claude-opus", Vendor::Anthropic, 5.0, 25.0, 0.50, 6.25),
    // Sonnet 档取 4.6 的 3/15 而不是 Sonnet 5 的 2/10：偏高原则。
    rcw("claude-sonnet", Vendor::Anthropic, 3.0, 15.0, 0.30, 3.75),
    rcw("claude-haiku", Vendor::Anthropic, 1.0, 5.0, 0.10, 1.25),
    rcw("claude-fable", Vendor::Anthropic, 10.0, 50.0, 1.00, 12.50),
    rcw("claude-mythos", Vendor::Anthropic, 10.0, 50.0, 1.00, 12.50),
    // 裸 `claude`：将来出现新档位（非 opus/sonnet/haiku/fable/mythos）时兜住，取家族最贵现役档。
    // 旧表这一格是 3/15，于是 `claude-fable-5`（真价 10/50）被低估到 30%。
    rcw("claude", Vendor::Anthropic, 10.0, 50.0, 1.00, 12.50),

    // ======================= OpenAI =======================
    // 来源：https://developers.openai.com/api/docs/pricing（2026-08-25；
    // 旧的 platform.openai.com/docs/pricing 301 到这里）。standard tier / short context。
    // 缓存命中比例**逐族不同**，故大量显式给 cache_read —— 旧实现写死 ÷10 正是这里最错。
    rcw("gpt-5-6-sol", Vendor::OpenAi, 4.00, 20.00, 0.40, 5.00),
    rcw("gpt-5-6-terra", Vendor::OpenAi, 2.00, 12.00, 0.20, 2.50),
    rcw("gpt-5-6-luna", Vendor::OpenAi, 0.20, 1.20, 0.02, 0.25),
    rcw("gpt-5-6-cyber", Vendor::OpenAi, 12.50, 75.00, 1.25, 15.625),
    rc("gpt-5-5-pro", Vendor::OpenAi, 30.00, 180.00, 3.00),
    rc("gpt-5-5-cyber", Vendor::OpenAi, 12.50, 75.00, 1.25),
    rc("gpt-5-5", Vendor::OpenAi, 5.00, 30.00, 0.50),
    rc("gpt-5-4-pro", Vendor::OpenAi, 30.00, 180.00, 3.00),
    rc("gpt-5-4-mini", Vendor::OpenAi, 0.75, 4.50, 0.075),
    rc("gpt-5-4-nano", Vendor::OpenAi, 0.20, 1.25, 0.02),
    rc("gpt-5-4", Vendor::OpenAi, 2.50, 15.00, 0.25),
    rc("gpt-5-3-codex", Vendor::OpenAi, 1.75, 14.00, 0.175),
    rc("gpt-5-2-pro", Vendor::OpenAi, 21.00, 168.00, 2.10),
    rc("gpt-5-2", Vendor::OpenAi, 1.75, 14.00, 0.175),
    rc("gpt-5-1", Vendor::OpenAi, 1.25, 10.00, 0.125),
    rc("gpt-5-pro", Vendor::OpenAi, 15.00, 120.00, 1.50),
    rc("gpt-5-mini", Vendor::OpenAi, 0.25, 2.00, 0.025),
    rc("gpt-5-nano", Vendor::OpenAi, 0.05, 0.40, 0.005),
    rc("gpt-5-search", Vendor::OpenAi, 1.25, 10.00, 0.125),
    rc("gpt-5", Vendor::OpenAi, 1.25, 10.00, 0.125),
    rc("chat-latest", Vendor::OpenAi, 5.00, 30.00, 0.50),
    // o 系：缓存命中是 0.25×~0.5×，不是 0.1×
    rc("o1-pro", Vendor::OpenAi, 150.00, 600.00, 15.00),
    // `o1-mini` 必须**显式列出**，否则它按最长片段落到下面那行 `o1` 上 → $15/$60，
    // 而真实价是 $1.10/$4.40（**13.6 倍高估**，且 tag 是 Exact、界面不打 ≈）。
    // 这与本轮修掉的 `gpt-4.1-nano → gpt-4`（输入价 100 倍）是同一个机制：
    // 「短片段是长名字的前缀」时，缺一行就等于继承贵档的价。
    // 它已不在官方现行价表上（故 retired），价取自下线前的公开价。
    retired(rc("o1-mini", Vendor::OpenAi, 1.10, 4.40, 0.55)),
    rc("o1", Vendor::OpenAi, 15.00, 60.00, 7.50),
    rc("o3-pro", Vendor::OpenAi, 20.00, 80.00, 2.00),
    rc("o3-mini", Vendor::OpenAi, 1.10, 4.40, 0.55),
    rc("o3", Vendor::OpenAi, 2.00, 8.00, 0.50),
    rc("o4-mini", Vendor::OpenAi, 1.10, 4.40, 0.275),
    // 4.1 / 4o / legacy —— 旧实现把 gpt-4.1* 全按 `gpt-4` 的 10/30 算：
    // nano 的输入价被高估 **100 倍**，mini 25 倍，4o-mini 16.7 倍。
    rc("gpt-4-1-nano", Vendor::OpenAi, 0.10, 0.40, 0.025),
    rc("gpt-4-1-mini", Vendor::OpenAi, 0.40, 1.60, 0.10),
    rc("gpt-4-1", Vendor::OpenAi, 2.00, 8.00, 0.50),
    rc("gpt-4o-mini", Vendor::OpenAi, 0.15, 0.60, 0.075),
    rc("gpt-4o", Vendor::OpenAi, 2.50, 10.00, 1.25),
    retired(r("gpt-4-turbo", Vendor::OpenAi, 10.00, 30.00)),
    retired(r("gpt-4", Vendor::OpenAi, 30.00, 60.00)),
    retired(r("gpt-3-5-turbo", Vendor::OpenAi, 0.50, 1.50)),
    // 纯 `codex-*` 私有别名兜底（`gpt-5.1-codex-max` 之类已被 `gpt-5-1` 命中）
    rc("codex", Vendor::OpenAi, 1.75, 14.00, 0.175),
    // `gpt-5-codex` 必须有**自己的行**：不然它同时命中 `gpt-5`(5) 与 `codex`(5)、长度打平，
    // 而平局在旧写法里由表序决出胜者 —— 表序又偷偷回到语义里。给它一条显式行既消掉平局，
    // 又让语义正确（它是 codex 调优版，按 codex 价而不是 gpt-5 价）。
    rc("gpt-5-codex", Vendor::OpenAi, 1.75, 14.00, 0.175),
    // 家族兜底 = 现役旗舰 gpt-5.6-sol。**旧表压根没有 `gpt` 这一格**，
    // 于是 `gpt-5` 既不含 `gpt-4o` 也不含 `gpt-4` → 整个 gpt-5/o 系全落 Unknown。
    rcw("gpt", Vendor::OpenAi, 4.00, 20.00, 0.40, 5.00),

    // ======================= Google Gemini =======================
    // 来源：https://ai.google.dev/gemini-api/docs/pricing（2026-08-25）。
    // Pro 类按 200k 分档，取**贵档**；Flash-Lite 按 text/image/video 档（我们只转文本）。
    r("gemini-3-7-flash", Vendor::Google, 0.75, 3.75),
    r("gemini-3-6-flash", Vendor::Google, 0.75, 3.75),
    r("gemini-3-5-flash-lite", Vendor::Google, 0.30, 2.50),
    r("gemini-3-5-flash", Vendor::Google, 1.50, 9.00),
    r("gemini-3-1-flash-lite", Vendor::Google, 0.25, 1.50),
    r("gemini-3-1-pro", Vendor::Google, 4.00, 18.00),
    r("gemini-3-pro", Vendor::Google, 4.00, 18.00),
    r("gemini-3-flash", Vendor::Google, 0.50, 3.00),
    r("gemini-2-5-pro", Vendor::Google, 2.50, 15.00),
    // 旧表用裸 `gemini` 的 1.25/5 算 flash-lite → 高估 12.5 倍。
    r("gemini-2-5-flash-lite", Vendor::Google, 0.10, 0.40),
    r("gemini-2-5-flash", Vendor::Google, 0.30, 2.50),
    r("gemini", Vendor::Google, 4.00, 18.00),

    // ======================= DeepSeek =======================
    // 来源：https://api-docs.deepseek.com/quick_start/pricing（2026-08-25）。峰/谷两档，取**峰价**。
    // 缓存命中约 0.033×（不是 0.1×）—— 写死 0.1 会高估 3 倍。
    rc("deepseek-v4-flash-vision", Vendor::DeepSeek, 0.44, 1.32, 0.014),
    rc("deepseek-v4-flash", Vendor::DeepSeek, 0.44, 1.32, 0.014),
    rc("deepseek-v4-pro", Vendor::DeepSeek, 1.32, 3.96, 0.044),
    rc("deepseek-v4", Vendor::DeepSeek, 1.32, 3.96, 0.044),
    // V3 时代名，已不在现行价页上；沿用历史价供对账，标退役。
    retired(rc("deepseek-chat", Vendor::DeepSeek, 0.27, 1.10, 0.027)),
    retired(rc("deepseek-reasoner", Vendor::DeepSeek, 0.55, 2.19, 0.055)),
    rc("deepseek", Vendor::DeepSeek, 1.32, 3.96, 0.044),

    // ======================= 智谱 GLM =======================
    // 来源：https://docs.z.ai/guides/overview/pricing（2026-08-25）。该页显式给 cached input（≈0.19×）。
    rc("glm-5-3", Vendor::Zhipu, 1.40, 4.40, 0.26),
    rc("glm-5-2", Vendor::Zhipu, 1.40, 4.40, 0.26),
    rc("glm-5-1", Vendor::Zhipu, 1.40, 4.40, 0.26),
    rc("glm-5-turbo", Vendor::Zhipu, 1.20, 4.00, 0.24),
    rc("glm-5v-turbo", Vendor::Zhipu, 1.20, 4.00, 0.24),
    rc("glm-5", Vendor::Zhipu, 1.00, 3.20, 0.20),
    rc("glm-4-7-flashx", Vendor::Zhipu, 0.07, 0.40, 0.01),
    rc("glm-4-7", Vendor::Zhipu, 0.60, 2.20, 0.11),
    rc("glm-4-6v-flashx", Vendor::Zhipu, 0.04, 0.40, 0.004),
    // ⚠️ **免费型号必须作为具体行进表**，不能靠一张单独的 FREE 名单做 `contains` 判断。
    // 第一版就是那么写的，而 `glm-4.7-flashx`（**收费**）的名字里含 `glm-4-7-flash`，
    // 于是它被判成免费、金额恒为 $0。放进表里之后由「最长片段命中」自然区分：
    // `glm-4-7-flashx`(14) 比 `glm-4-7-flash`(13) 长，赢。
    // 来源：docs.z.ai/guides/overview/pricing 标注 Free（2026-08-25）。
    r("glm-4-7-flash", Vendor::Zhipu, 0.0, 0.0),
    r("glm-4-6v-flash", Vendor::Zhipu, 0.0, 0.0),
    r("glm-4-5-flash", Vendor::Zhipu, 0.0, 0.0),
    rc("glm-4-6v", Vendor::Zhipu, 0.30, 0.90, 0.05),
    rc("glm-4-6", Vendor::Zhipu, 0.60, 2.20, 0.11),
    rc("glm-4-5-airx", Vendor::Zhipu, 1.10, 4.50, 0.22),
    rc("glm-4-5-air", Vendor::Zhipu, 0.20, 1.10, 0.03),
    rc("glm-4-5-x", Vendor::Zhipu, 2.20, 8.90, 0.45),
    rc("glm-4-5v", Vendor::Zhipu, 0.60, 1.80, 0.11),
    rc("glm-4-5", Vendor::Zhipu, 0.60, 2.20, 0.11),
    rc("glm-4-32b", Vendor::Zhipu, 0.10, 0.10, 0.01),
    rc("glm-4", Vendor::Zhipu, 0.60, 2.20, 0.11),
    rc("glm", Vendor::Zhipu, 1.40, 4.40, 0.26),

    // ======================= 月之暗面 Kimi =======================
    // 来源：https://platform.kimi.ai/docs/pricing/chat-k3 与 /chat-k26（2026-08-25；
    // platform.moonshot.ai 301 到 platform.kimi.ai）。
    rc("kimi-k3", Vendor::Moonshot, 3.00, 15.00, 0.30),
    rc("kimi-k2-7-code", Vendor::Moonshot, 0.95, 4.00, 0.19),
    rc("kimi-k2-6", Vendor::Moonshot, 0.95, 4.00, 0.16),
    rc("kimi", Vendor::Moonshot, 3.00, 15.00, 0.30),
    // `moonshot-v1-*` 官方页标注即将下线、未列现价 → 按同厂旗舰偏高兜住。
    rc("moonshot", Vendor::Moonshot, 3.00, 15.00, 0.30),

    // ======================= 通义千问 Qwen =======================
    // 来源：OpenRouter 上 Alibaba 的**第一方**端点（2026-08-25）。
    // 阿里云自己的定价页实测拿不到数字（只有模型清单），故用第一方端点价，已在此注明。
    rcw("qwen3-8-max", Vendor::Alibaba, 2.00, 6.00, 0.25, 2.50),
    rc("qwen3-8", Vendor::Alibaba, 2.50, 6.25, 0.50),
    rc("qwen3-7-max", Vendor::Alibaba, 1.25, 3.75, 0.13),
    // ≥256k 长档（偏高原则；标准档是 0.32/1.28）
    rc("qwen3-7-plus", Vendor::Alibaba, 0.96, 3.84, 0.10),
    rc("qwen3-6-plus", Vendor::Alibaba, 0.50, 3.00, 0.05),
    rc("qwen3-5", Vendor::Alibaba, 0.60, 3.60, 0.35),
    // ⚠️ qwen-max / plus / turbo / long / qwen3-coder 等老型号**未取证**：真价比兜底低，
    // 但没有权威来源就不写数，让它们被兜底行高估并在界面打 ≈ 如实标注。
    rcw("qwen", Vendor::Alibaba, 2.00, 6.00, 0.25, 2.50),

    // ======================= xAI Grok =======================
    // 来源：https://docs.x.ai/docs/models（2026-08-25）。按 prompt ≥200k 分档，取**贵档**。
    rc("grok-4-6", Vendor::XAi, 4.00, 12.00, 1.00),
    rc("grok-4-5", Vendor::XAi, 4.00, 12.00, 0.60),
    rc("grok-4-3", Vendor::XAi, 2.50, 5.00, 0.40),
    rc("grok-4-20", Vendor::XAi, 2.50, 5.00, 0.40),
    rc("grok-build", Vendor::XAi, 2.00, 4.00, 0.40),
    // 旧表是 3/15，把现役 4.6（4/12）的输出价高估了 25%。
    rc("grok", Vendor::XAi, 4.00, 12.00, 1.00),

    // ======================= Mistral =======================
    // 来源：https://mistral.ai/pricing/api（2026-08-25）。缓存 -90% → 0.1×，用默认。
    r("mistral-medium", Vendor::Mistral, 1.50, 7.50),
    r("mistral-large", Vendor::Mistral, 0.50, 1.50),
    r("mistral-small", Vendor::Mistral, 0.15, 0.60),
    r("ministral-3", Vendor::Mistral, 0.20, 0.20),
    r("ministral", Vendor::Mistral, 0.20, 0.20),
    r("codestral", Vendor::Mistral, 0.30, 0.90),
    r("mistral", Vendor::Mistral, 1.50, 7.50),
    // ⚠️ magistral / devstral / open-mistral-nemo：未取证，刻意不给行。

    // ======================= MiniMax =======================
    // 来源：OpenRouter 上 Minimax 第一方端点（2026-08-25；官方 price 页 404）。
    rc("minimax-m3", Vendor::MiniMax, 0.30, 1.20, 0.06),
    rc("minimax-m2", Vendor::MiniMax, 0.30, 1.20, 0.06),
    rc("minimax", Vendor::MiniMax, 0.30, 1.20, 0.06),

    // ======================= 腾讯混元 =======================
    // 来源：OpenRouter 上 Tencent 端点（2026-08-25）。
    // ⚠️ 现行 id 是 `hy3`，**不含** "hunyuan" —— 只留 hunyuan 片段会全部漏掉。
    rc("hy3", Vendor::Tencent, 0.132, 0.528, 0.033),
    rc("hy-mt2", Vendor::Tencent, 0.074, 0.295, 0.019),
    rc("hunyuan", Vendor::Tencent, 0.132, 0.528, 0.033),

    // ======================= 字节 Seed =======================
    // 来源：OpenRouter 上 Seed 端点（2026-08-25；火山官方文档抓取被拦）。
    r("seed-2-1-turbo", Vendor::ByteDance, 0.50, 2.50),
    r("seed-2-0-code", Vendor::ByteDance, 0.50, 3.00),
    r("seed-2", Vendor::ByteDance, 0.50, 3.00),
    // ⚠️ `doubao-*` 官方价**未取证**（volcengine 抓取被拦），刻意不给行 → 落 Unknown、
    // 界面点出模型名。按同厂 Seed 价兜住会给出一个用户会当真的数，而它没有来源支撑。

    // ======================= Cohere =======================
    // 来源：OpenRouter 的 Cohere 端点（command-a）+ cohere.com/pricing 的 legacy 表（2026-08-25）。
    r("command-a", Vendor::Cohere, 2.50, 10.00),
    retired(r("command-r-plus", Vendor::Cohere, 2.50, 10.00)),
    retired(r("command-r", Vendor::Cohere, 0.50, 1.50)),
    retired(r("command-light", Vendor::Cohere, 0.30, 0.60)),
    r("command", Vendor::Cohere, 2.50, 10.00),
    // ⚠️ Command A+ / A Reasoning / A Translate / A Vision：未取证（官网只留 legacy 表）。

    // ======================= Perplexity Sonar =======================
    // 来源：https://docs.perplexity.ai/getting-started/pricing（2026-08-25）。
    // ⚠️ Sonar 另有**按请求**计的检索费（每千请求数美元），token 口径永远算不进去 ——
    // 故 `super::sonar_undercounts_by_design` 那条注释要求界面如实声明「Sonar 的金额一定低于实际账单」。
    r("sonar-deep-research", Vendor::Perplexity, 2.00, 8.00),
    r("sonar-reasoning-pro", Vendor::Perplexity, 2.00, 8.00),
    r("sonar-pro", Vendor::Perplexity, 3.00, 15.00),
    r("sonar", Vendor::Perplexity, 3.00, 15.00),

    // ======================= 开放权重（托管市场价，非厂商官方价） =======================
    // 来源：https://www.together.ai/pricing（2026-08-25）。这些模型没有「官方价」，
    // 各托管方自定；取一个主流托管方的价并在此注明，界面照常打 ≈。
    r("gpt-oss-120b", Vendor::OpenWeight, 0.15, 0.60),
    r("gpt-oss-20b", Vendor::OpenWeight, 0.05, 0.20),
    r("gpt-oss", Vendor::OpenWeight, 0.15, 0.60),
    r("llama-3-3-70b", Vendor::OpenWeight, 1.04, 1.04),
    r("llama-3-8b", Vendor::OpenWeight, 0.14, 0.14),
    r("llama", Vendor::OpenWeight, 1.04, 1.04),
    r("gemma-4", Vendor::OpenWeight, 0.39, 0.97),
    r("gemma", Vendor::OpenWeight, 0.39, 0.97),
    r("nemotron", Vendor::OpenWeight, 0.60, 3.60),
    // ⚠️ 刻意**不给行**（未取证，落 Unknown）：百度文心 ernie-*、阶跃星辰 step-*、
    // 零一万物 yi-*、字节豆包 doubao-*、百川 baichuan*、讯飞 spark-*、qwq-*、phi-*、nova-*。
];

/// 明确**免费**的模型片段。它们同时作为零价具体行在 [`PRICING_TABLE`] 里 ——
/// 这张名单只用于 `free_models_are_zero_rows_in_the_table` 那条不变量校验，
/// 确保「声称免费」与「表里真的是 0」不会分叉（生产查价压根不读它，故标 dead_code）。
///
/// 来源：docs.z.ai/guides/overview/pricing 标注 Free（2026-08-25）。
#[allow(dead_code)]
pub(crate) const FREE_MODELS: &[&str] = &["glm-4-7-flash", "glm-4-5-flash", "glm-4-6v-flash"];

/// 家族兜底行 ↔ 旗舰具体行的**显式绑定**。
///
/// `family_default_equals_designated_flagship` 与 `flagship_row_must_be_live` 两条测试校验它。
/// **加新世代时改这里**；忘了改而新世代价格又不同 → 测试直接红。
/// 这是防「兜底行停在退役价」（本表上一版 3× 高估的根因）的机械守门人。
pub(crate) const FAMILY_FLAGSHIP: &[(&str, &str)] = &[
    ("claude-opus", "claude-opus-5"),
    ("claude-sonnet", "claude-sonnet-4-6"),
    ("claude-haiku", "claude-haiku-4-5"),
    ("claude-fable", "claude-fable-5"),
    ("claude-mythos", "claude-mythos-5"),
    ("claude", "claude-fable-5"),
    ("gpt", "gpt-5-6-sol"),
    ("codex", "gpt-5-3-codex"),
    ("gemini", "gemini-3-1-pro"),
    ("deepseek", "deepseek-v4-pro"),
    ("glm", "glm-5-3"),
    ("kimi", "kimi-k3"),
    ("moonshot", "kimi-k3"),
    ("qwen", "qwen3-8-max"),
    ("grok", "grok-4-6"),
    ("mistral", "mistral-medium"),
    ("minimax", "minimax-m3"),
    ("hunyuan", "hy3"),
    ("command", "command-a"),
    ("sonar", "sonar-pro"),
    ("llama", "llama-3-3-70b"),
    ("gemma", "gemma-4"),
    ("gpt-oss", "gpt-oss-120b"),
];
