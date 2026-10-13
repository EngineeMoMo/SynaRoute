//! 在线单价表：每天从 LiteLLM 公开的价格表（经 jsDelivr）拉一份，补内置表的空白。
//!
//! # 它补的缺口
//!
//! 内置表（[`super::table`]）靠人工逐家核对官方定价页，准但会旧：新模型一发布，
//! 在我们出下一版之前它只能落到家族兜底（界面标 `≈`），或者更糟 —— 被钉在一个更短的
//! **具体**行上（`gpt-6-astra-mini` 命中 `gpt-6-astra`，按 10/50 算，界面还不标 `≈`）。
//! 用户 2026-10-13 的原话是「最好价格表能自己去查，不用每次都更新」。
//!
//! # 合并规则：最长片段命中，平局内置赢
//!
//! 与内置表同一套「归一 + 最长片段命中」，两张表的候选放在一起比：
//! - **平局（同名）内置赢。** 内置表是逐条对过官方页的；2026-10-13 实测两边共有的 108 个
//!   型号里 101 个一致，不一致的 7 个（DeepSeek、Qwen 区域价、Mistral、MiniMax 缓存价）
//!   逐条以官方页为准。
//! - **在线只在命中的片段严格更长时才赢** —— 也就是它认得一个内置表不认得的、更具体的名字：
//!   日期快照（`gpt-4o-2024-05-13` 当年是 5/15，内置按 `gpt-4o` 的 2.5/10 算会低估一半）、
//!   新子型号、整族缺行（`qwen-plus` / `qwen-turbo` 内置表没取证）。
//!
//! 三条刻意的边界：
//! - 在线行一律算**精确命中**（`PricingSource::Exact`）。它是那个具体型号的价，不参与家族兜底；
//!   认不出的将来型号仍落内置的家族兜底（= 现役旗舰价，偏高原则不变）。
//! - 在线片段必须从**词首**开始命中（名字开头或紧跟 `-`），内置表没有这道限制。在线表有几百行
//!   没人逐条看过，「短片段从词中间吃掉别家模型名」这类问题没法像内置表那样靠测试逐条钉住。
//! - 在线行缺缓存价时，借内置表同名命中行的缓存比例（见 [`resolve`]）。
//!
//! # 只认厂商自己的 API
//!
//! LiteLLM 里同一个模型挂着几十个转售渠道（bedrock / azure / vertex / openrouter…），价各不相同。
//! 按名字前缀认厂商（[`VENDORS`]），再要求 `litellm_provider` 是**那家自己的** API，其余一律丢弃。
//! 分档计价取「门槛 ≤256K 的最贵一档」：这个阈值恰好复现内置表的全部取舍
//! （Haiku 100K、Gemini/Grok 200K、Qwen 256K 取贵档；OpenAI 272K、MiniMax 512K 不取）。
//!
//! # 失败
//!
//! 拉不到、格式变了、条数异常 → **保留上一份**（内存里的，或磁盘上的 `pricing-online.json`），
//! 只在状态里记一句原因（用量页与诊断报告会显示）。**不落事件、不弹通知**：jsDelivr 在部分网络下
//! 时好时坏，同 `update_watch` 的取舍。一份都没有时就是纯内置表，与之前完全一样。
//!
//! 出站流量只有一种：每天一次对公开 CDN 的匿名 GET，拉一个公开的 JSON。不带任何本机数据、
//! 密钥或用量。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{best_builtin, mtok_to_ntx, normalize, ModelPricing};

/// 镜像，按序尝试，第一个「200 且解析得出一张合格表」的胜出。
///
/// 顺序按 2026-10-13 本机实测：`cdn` / `gcore` / `testingcf` 都在 2.5s 内返回（gzip 后约 133 KB），
/// `fastly` 25s 超时，`raw.githubusercontent.com` 本机 DNS 直接解析不出。后两个排在最后，
/// 只为那些 jsDelivr 恰好被挡、而 GitHub 能通的网络兜底。
const MIRRORS: &[&str] = &[
    "https://cdn.jsdelivr.net/gh/BerriAI/litellm@main/model_prices_and_context_window.json",
    "https://gcore.jsdelivr.net/gh/BerriAI/litellm@main/model_prices_and_context_window.json",
    "https://testingcf.jsdelivr.net/gh/BerriAI/litellm@main/model_prices_and_context_window.json",
    "https://fastly.jsdelivr.net/gh/BerriAI/litellm@main/model_prices_and_context_window.json",
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json",
];

/// 筛选规则的版本。**改了 [`accept`] 的判据就要加一**：磁盘缓存只存筛选后的结果，
/// 规则变了而沿用旧缓存，等于新规则要等到下一次拉取成功才生效（墙内用户可能要很久）。
/// 缓存价的补全（[`resolve`]）每次装载时现算、不落盘，改它不必加一。
pub(crate) const RULES_VERSION: u32 = 1;
const FILE_VERSION: u32 = 1;
const FILE_NAME: &str = "pricing-online.json";

/// 分档计价取档的门槛上限（token）。取值理由见模块头「只认厂商自己的 API」。
const TIER_CAP_TOKENS: f64 = 256_000.0;
/// 单价上界（$/MTok）。现役最贵的输出价是几百美元（`o1-pro` 600），超过这个只可能是数据错了。
const MAX_PRICE_PER_MTOK: f64 = 1000.0;
/// 筛完少于这么多行就整份拒绝。2026-10-13 实测 323 行；少到这个量级只可能是上游改了格式，
/// 而那时宁可沿用上一份，也不要拿一张残表去替换它。
const MIN_ROWS: usize = 50;
/// 响应体上限（解压后）。实测 2.7 MB，留十倍余量；超了按坏数据处理、不再往下读。
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// 刷新周期。jsDelivr 自己的边缘缓存是 12 小时（`s-maxage=43200`），一天一次足够跟上。
const REFRESH_MS: i64 = 24 * 3600 * 1000;
/// 后台线程的检查节奏。失败时也按这个节奏重试 —— 一天最多 24 轮，每轮至多试一遍全部镜像。
const CHECK_EVERY: Duration = Duration::from_secs(3600);
/// 启动后先等一会儿再拉：别和启动时的更新检查、健康探测、代理自启动抢网络。
const FIRST_DELAY: Duration = Duration::from_secs(30);

/// 名字前缀 → 认可的 `litellm_provider`（**厂商自己的 API**）。前缀按归一后的名字匹配。
///
/// 不在这里的厂商（腾讯混元、开源权重模型等）在 LiteLLM 里没有第一方条目，只走内置表。
const VENDORS: &[(&[&str], &[&str])] = &[
    (&["claude"], &["anthropic"]),
    (&["gpt", "chatgpt", "codex", "o1", "o3", "o4"], &["openai"]),
    (&["gemini"], &["gemini"]),
    (&["deepseek"], &["deepseek"]),
    (&["glm"], &["zai"]),
    (&["kimi", "moonshot"], &["moonshot"]),
    (&["qwen", "qwq"], &["dashscope"]),
    (&["grok"], &["xai"]),
    (
        &["mistral", "ministral", "codestral", "devstral", "magistral", "pixtral", "open-mistral", "open-mixtral"],
        &["mistral"],
    ),
    (&["minimax"], &["minimax"]),
    (&["command"], &["cohere_chat", "cohere"]),
    (&["sonar"], &["perplexity"]),
    (&["doubao", "seed"], &["volcengine"]),
];

/// `bare` 是不是以这个前缀「开头的那一族」：等于它、或紧跟 `-`、或（前缀长于 2 时）直接连写。
///
/// 两字符前缀（`o1` / `o3` / `o4`）只认前两种：否则 `o3` 会认领任何以这两个字母开头的名字。
fn claims(prefix: &str, bare: &str) -> bool {
    bare == prefix
        || bare.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('-'))
        || (prefix.len() > 2 && bare.starts_with(prefix))
}

fn providers_for(bare: &str) -> Option<&'static [&'static str]> {
    VENDORS
        .iter()
        .find(|(prefixes, _)| prefixes.iter().any(|p| claims(p, bare)))
        .map(|(_, providers)| *providers)
}

/// 筛选后的一行（单位 $/MTok，与内置表同口径）。磁盘缓存存的就是它。
///
/// 缓存价 `None` = 上游没给（或给了 0），装载时按内置表补（[`resolve`]）。**刻意不落盘补全后的值**：
/// 内置表随版本更新，补全口径也该跟着变，而不是停在拉取那一刻。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct OnlineRow {
    pub(crate) frag: String,
    pub(crate) input: f64,
    pub(crate) output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cache_write: Option<f64>,
}

/// 把 LiteLLM 的整份 JSON 筛成「厂商自己报的价」。`Err` = 这份不可用（调用方保留上一份）。
pub(crate) fn parse(body: &[u8]) -> Result<Vec<OnlineRow>, String> {
    let root: Value = serde_json::from_slice(body).map_err(|e| format!("不是合法 JSON：{e}"))?;
    let Value::Object(entries) = root else {
        return Err("顶层不是对象（格式可能变了）".into());
    };
    let mut best: BTreeMap<String, OnlineRow> = BTreeMap::new();
    for (id, entry) in &entries {
        let Some(row) = entry.as_object().and_then(|m| accept(id, m)) else { continue };
        // 同一个名字出现多次（`openai/x` 与 `x` 都是第一方）时留**贵的**：偏高原则。
        match best.get(&row.frag) {
            Some(prev) if (prev.input, prev.output) >= (row.input, row.output) => {}
            _ => {
                best.insert(row.frag.clone(), row);
            }
        }
    }
    if best.len() < MIN_ROWS {
        return Err(format!("只认出 {} 个模型（少于 {MIN_ROWS}），数据源格式可能变了", best.len()));
    }
    Ok(best.into_values().collect())
}

/// 一条条目要不要，要的话折算成一行。判据见模块头；**改这里要加 [`RULES_VERSION`]**。
fn accept(id: &str, m: &Map<String, Value>) -> Option<OnlineRow> {
    // 只要对话类。embedding / 图像 / 语音 / moderation 的计价单位与语义都不同，混进来会被
    // 同名对话模型的最长命中误用。没写 `mode` 的条目照收（实测都是 Fireworks 的按尺寸分档，
    // 认不出厂商、本来也进不来）。
    if !matches!(m.get("mode").and_then(Value::as_str), None | Some("chat") | Some("responses")) {
        return None;
    }
    let bare = normalize(id.rsplit('/').next().unwrap_or(id));
    if bare.is_empty() || !bare.is_ascii() {
        return None;
    }
    let provider = m.get("litellm_provider").and_then(Value::as_str)?;
    if !providers_for(&bare)?.contains(&provider) {
        return None;
    }

    let per = |k: &str| m.get(k).and_then(Value::as_f64).map(|x| x * 1e6);
    let (mut input, mut output) = (per("input_cost_per_token"), per("output_cost_per_token"));
    let (mut cr, mut cw) = (per("cache_read_input_token_cost"), per("cache_creation_input_token_cost"));
    if let Some(suffix) = top_tier_suffix(m) {
        input = per(&format!("input_cost_per_token{suffix}")).or(input);
        output = per(&format!("output_cost_per_token{suffix}")).or(output);
        cr = per(&format!("cache_read_input_token_cost{suffix}")).or(cr);
        cw = per(&format!("cache_creation_input_token_cost{suffix}")).or(cw);
    }
    // 另一种分档写法（DashScope 用）：`tiered_pricing: [{range: [起, 止], ...}]`。同一阈值规则。
    if let Some(t) = top_tiered_entry(m) {
        let per_t = |k: &str| t.get(k).and_then(Value::as_f64).map(|x| x * 1e6);
        input = per_t("input_cost_per_token").or(input);
        output = per_t("output_cost_per_token").or(output);
        cr = per_t("cache_read_input_token_cost").or(cr);
    }
    let (input, output) = (input?, output?);

    let sane = |x: f64| x.is_finite() && (0.0..=MAX_PRICE_PER_MTOK).contains(&x);
    if ![Some(input), Some(output), cr, cw].into_iter().flatten().all(sane) {
        return None;
    }
    // 两个都是 0 = 「还没定价」的占位，不是免费 —— 真免费的型号内置表里有显式的 0 价行。
    if input <= 0.0 && output <= 0.0 {
        return None;
    }
    // 缓存价给 0 在这份数据里是「没填」（34 条 `cache_creation_input_token_cost: 0` 全是不单独
    // 收写入费的家族），按「缺」处理，装载时按内置表补。照字面当 0 会让缓存 token 白嫖。
    Some(OnlineRow {
        frag: bare,
        input,
        output,
        cache_read: cr.filter(|x| *x > 0.0),
        cache_write: cw.filter(|x| *x > 0.0),
    })
}

/// `*_above_<N>k_tokens` 形态里 N ≤ 256 的最大一档，返回它的后缀（如 `_above_200k_tokens`）。
fn top_tier_suffix(m: &Map<String, Value>) -> Option<String> {
    m.keys()
        .filter_map(|k| k.strip_prefix("input_cost_per_token_above_")?.strip_suffix("k_tokens")?.parse::<u32>().ok())
        .filter(|n| f64::from(*n) <= TIER_CAP_TOKENS / 1000.0)
        .max()
        .map(|n| format!("_above_{n}k_tokens"))
}

/// `tiered_pricing` 里起点 ≤256K 的最高一档。
fn top_tiered_entry(m: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let start = |t: &Map<String, Value>| t.get("range").and_then(|r| r.get(0)).and_then(Value::as_f64).unwrap_or(0.0);
    m.get("tiered_pricing")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter(|t| start(t) <= TIER_CAP_TOKENS)
        .max_by(|a, b| start(a).total_cmp(&start(b)))
}

/// 内存里的一行：缓存价已补全、已换算成纳美元/token。查价热路径上只碰它。
#[derive(Debug, Clone)]
pub(crate) struct Priced {
    pub(crate) frag: String,
    pub(crate) pricing: ModelPricing,
}

/// 补全缓存价并换算单位。
///
/// 缺缓存价（实测 323 行里 95 行缺命中价、绝大多数缺写入价）时，**借内置表同名命中行的比例**：
/// 同一家族的缓存折扣通常一致（`qwen-plus` 借 `qwen` 的 0.125×，`gpt-4o-2024-05-13` 借 `gpt-4o`
/// 的 0.5×）。内置表也认不出这个名字时按输入价算 —— 偏高原则，不猜一个折扣出来。
pub(crate) fn resolve(row: &OnlineRow) -> Priced {
    let base = best_builtin(&row.frag).map(ModelPricing::from_row);
    let ratio = |pick: fn(&ModelPricing) -> u64| match base {
        Some(b) if b.input_per_ntx > 0 => pick(&b) as f64 / b.input_per_ntx as f64,
        _ => 1.0,
    };
    let input = mtok_to_ntx(row.input);
    let fill = |explicit: Option<f64>, pick: fn(&ModelPricing) -> u64| match explicit {
        Some(v) => mtok_to_ntx(v),
        None => (input as f64 * ratio(pick)).round() as u64,
    };
    Priced {
        frag: row.frag.clone(),
        pricing: ModelPricing {
            input_per_ntx: input,
            output_per_ntx: mtok_to_ntx(row.output),
            cache_read_per_ntx: fill(row.cache_read, |p| p.cache_read_per_ntx),
            cache_creation_per_ntx: fill(row.cache_write, |p| p.cache_creation_per_ntx),
        },
    }
}

/// 当前生效的一份在线表。
pub(crate) struct Snapshot {
    pub(crate) rows: Vec<Priced>,
    pub(crate) fetched_at_ms: i64,
    /// 从哪个镜像拉的（只记主机名）。
    pub(crate) source: String,
}

struct State {
    table: Option<Arc<Snapshot>>,
    /// 最近一次刷新失败的原因。成功即清空；有它且 `table` 也在 = 正在沿用上一份。
    last_error: Option<String>,
}

static STATE: parking_lot::RwLock<State> = parking_lot::RwLock::new(State { table: None, last_error: None });

/// 查价热路径用：只在读锁里克隆一个 `Arc`，扫描在锁外做。
pub(crate) fn current() -> Option<Arc<Snapshot>> {
    STATE.read().table.clone()
}

/// 装上一份新表并清掉失败原因。
fn install(rows: &[OnlineRow], fetched_at_ms: i64, source: &str) {
    // 片段只收 ASCII：`match_at` 逐字节推进，非 ASCII 片段会在字符中间切片。`accept` 已经挡了，
    // 这里防的是被手改过的磁盘缓存。
    let rows = rows.iter().filter(|r| !r.frag.is_empty() && r.frag.is_ascii()).map(resolve).collect();
    let snapshot = Arc::new(Snapshot { rows, fetched_at_ms, source: source.to_string() });
    let mut st = STATE.write();
    st.table = Some(snapshot);
    st.last_error = None;
}

// ---------------- 磁盘缓存 ----------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnDisk {
    version: u32,
    rules_version: u32,
    fetched_at_ms: i64,
    source: String,
    rows: Vec<OnlineRow>,
}

/// `pricing-online.json`：与 `config.json` 同目录（同 `usage-keys.json` 的取舍）。
fn file_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(FILE_NAME)
}

/// 读不出、版本不对、规则版本不对、行数异常 → `None`。这只是缓存，下一次拉取成功就会重写。
fn load(path: &Path) -> Option<OnDisk> {
    let raw = std::fs::read(path).ok()?;
    let d: OnDisk = serde_json::from_slice(&raw).ok()?;
    (d.version == FILE_VERSION && d.rules_version == RULES_VERSION && d.rows.len() >= MIN_ROWS).then_some(d)
}

fn save(path: &Path, d: &OnDisk) {
    match serde_json::to_vec(d) {
        Ok(bytes) => {
            if let Err(e) = crate::secret::atomic_write(path, &bytes) {
                tracing::warn!("写 {FILE_NAME} 失败（下次启动要重新拉取在线单价）: {e}");
            }
        }
        Err(e) => tracing::warn!("序列化 {FILE_NAME} 失败: {e}"),
    }
}

// ---------------- 拉取 ----------------

/// 拉一个镜像的原始字节，响应体超过 `cap` 即中止。只负责「拿到字节」，不解析。
///
/// 边读边计数而不是读完再比：CDN 的 gzip 响应常常不带 `Content-Length`，读完再判就晚了。
async fn fetch_bytes(url: &str, cap: usize) -> Result<Vec<u8>, String> {
    let mut resp = crate::upstream::decoding_client()
        .get(url)
        .header("user-agent", concat!("SynaRoute/", env!("CARGO_PKG_VERSION")))
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > cap {
            return Err(format!("响应体超过 {cap} 字节"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 按序试镜像，第一个「拿到字节**且**解析得出合格表」的胜出，返回 (行, 镜像主机名)。
///
/// 解析失败也换下一个：一个镜像回 200 却是残缺/陈旧的文件，与它连不上一样不能用。
/// 全部失败时把每个镜像的原因拼起来（截到 600 字），状态里与诊断报告里都看得到。
pub(crate) async fn fetch_from(mirrors: &[&str], cap: usize) -> Result<(Vec<OnlineRow>, String), String> {
    let mut why = Vec::new();
    for url in mirrors {
        match fetch_bytes(url, cap).await.and_then(|b| parse(&b)) {
            Ok(rows) => return Ok((rows, host_of(url))),
            Err(e) => why.push(format!("{}：{e}", host_of(url))),
        }
    }
    let mut msg = why.join("；");
    if msg.chars().count() > 600 {
        msg = msg.chars().take(600).collect::<String>() + "…";
    }
    Err(msg)
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', ':']).next().unwrap_or(rest).to_string()
}

/// 一轮刷新的同步后半：成功就装上并落盘，失败只记原因、**不动现有的表**。
///
/// 拆出来是为了可测：它改的是进程级状态，测试要串行化；而网络那一半（[`fetch_from`]）不需要。
fn apply(result: Result<(Vec<OnlineRow>, String), String>, config_path: &Path, now_ms: i64) {
    match result {
        Ok((rows, source)) => {
            install(&rows, now_ms, &source);
            let d = OnDisk { version: FILE_VERSION, rules_version: RULES_VERSION, fetched_at_ms: now_ms, source, rows };
            save(&file_path(config_path), &d);
        }
        Err(e) => STATE.write().last_error = Some(e),
    }
}

/// 该不该刷新：还没有表，或表已超过一天。
///
/// 时间戳在**未来**（系统时钟回拨过）也算该刷新：否则一次回拨就让刷新停摆到时钟追上来为止。
/// `saturating_sub` 防的是被手改过的缓存里塞了一个离谱的时间戳 —— 普通减法在调试构建下会溢出崩溃。
fn is_due(fetched_at_ms: Option<i64>, now_ms: i64) -> bool {
    fetched_at_ms.map_or(true, |t| !(0..REFRESH_MS).contains(&now_ms.saturating_sub(t)))
}

/// 起后台刷新线程。在 `lib.rs` 里与 `balance_gate` / `codex_watch` 并列调一次。
///
/// 磁盘缓存**同步**装上再起线程：启动后的第一个请求就能用上次拉到的在线价，不必等那一轮网络。
/// 线程**刻意独立一趟**，理由同 `balance_gate`（搭别的定时车会被无关设置停掉、周期被牵着走）。
pub(crate) fn spawn_background(store: &crate::store::Store) {
    let config_path = PathBuf::from(store.config_path_display());
    if let Some(d) = load(&file_path(&config_path)) {
        install(&d.rows, d.fetched_at_ms, &d.source);
    }
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        rt.block_on(async move {
            tokio::time::sleep(FIRST_DELAY).await;
            loop {
                let fetched = current().map(|s| s.fetched_at_ms);
                if is_due(fetched, chrono::Utc::now().timestamp_millis()) {
                    let result = fetch_from(MIRRORS, MAX_BODY_BYTES).await;
                    apply(result, &config_path, chrono::Utc::now().timestamp_millis());
                }
                tokio::time::sleep(CHECK_EVERY).await;
            }
        });
    });
}

// ---------------- 状态（用量页 / 诊断报告） ----------------

/// 单价表现状，给用量页表头与诊断报告。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PricingStatus {
    /// 内置表的核对日期（`YYYY-MM-DD`）。
    pub(crate) builtin_date: &'static str,
    /// 在线表当前有几个模型。0 = 还没有在线表（从没拉成过，或磁盘缓存已失效）。
    pub(crate) online_rows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) online_fetched_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) online_source: Option<String>,
    /// 最近一次拉取失败的原因。与 `online_rows > 0` 同时出现 = 正在沿用上一份。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) online_error: Option<String>,
}

pub(crate) fn status() -> PricingStatus {
    let st = STATE.read();
    let t = st.table.as_ref();
    PricingStatus {
        builtin_date: super::PRICE_TABLE_VERIFIED_ON,
        online_rows: t.map_or(0, |s| s.rows.len()),
        online_fetched_at_ms: t.map(|s| s.fetched_at_ms),
        online_source: t.map(|s| s.source.clone()),
        online_error: st.last_error.clone(),
    }
}

/// 诊断报告里的一行。金额与账单对不上时，「当时用的是哪张表、多旧」是排查的第一个问题。
pub(crate) fn status_line() -> String {
    let s = status();
    let mut line = format!("内置表核对于 {}", s.builtin_date);
    match (s.online_rows, s.online_fetched_at_ms) {
        (n, Some(at)) if n > 0 => {
            let when = chrono::DateTime::from_timestamp_millis(at).map(|d| d.to_rfc3339()).unwrap_or_default();
            line += &format!("；在线表 {n} 个模型（{}，拉取于 {when}）", s.online_source.unwrap_or_default());
        }
        _ => line += "；无在线表",
    }
    if let Some(e) = s.online_error {
        line += &format!("；最近一次拉取失败：{e}");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TokenUsage;
    use crate::pricing::{estimate_cost, lookup_in, PricingSource};
    use serde_json::json;

    /// 串行化改进程级 `STATE` 的用例。别的查价用例用的都是真实模型名，撞不上这里装进去的
    /// `zq-synaroute-*` / `gpt-zq-*`，所以只有这几条之间需要互斥。
    static GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn snapshot_state() -> (Option<Arc<Snapshot>>, Option<String>) {
        let st = STATE.read();
        (st.table.clone(), st.last_error.clone())
    }
    fn restore_state(saved: (Option<Arc<Snapshot>>, Option<String>)) {
        let mut st = STATE.write();
        st.table = saved.0;
        st.last_error = saved.1;
    }

    fn chat(provider: &str, input: f64, output: f64) -> Value {
        json!({"litellm_provider": provider, "mode": "chat", "input_cost_per_token": input, "output_cost_per_token": output})
    }
    /// LiteLLM 形态的数据：`extra` 之外补够 `MIN_ROWS` 行合格的填充行（否则整份会被当成格式变了）。
    fn feed(extra: Value) -> Vec<u8> {
        let mut m = Map::new();
        for i in 0..MIN_ROWS {
            m.insert(format!("gpt-zq-filler-{i}"), chat("openai", 1e-6, 2e-6));
        }
        if let Value::Object(e) = extra {
            m.extend(e);
        }
        serde_json::to_vec(&Value::Object(m)).unwrap()
    }
    fn find<'a>(rows: &'a [OnlineRow], frag: &str) -> Option<&'a OnlineRow> {
        rows.iter().find(|r| r.frag == frag)
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }
    fn priced(frag: &str, input: f64, output: f64) -> Priced {
        resolve(&OnlineRow { frag: frag.into(), input, output, cache_read: None, cache_write: None })
    }

    /// 只认厂商自己的 API。注入判据：去掉 `accept` 里 `providers_for(..).contains(&provider)` 那道门 → 变红。
    #[test]
    fn only_the_vendors_own_api_is_accepted() {
        let rows = parse(&feed(json!({
            "claude-zq-9": chat("anthropic", 3e-6, 1.5e-5),
            "bedrock/claude-zq-8": chat("bedrock", 9e-6, 9e-5),
            "openrouter/gpt-zq-8": chat("openrouter", 9e-6, 9e-5),
            "llama-zq-1": chat("meta", 1e-6, 1e-6),
        })))
        .unwrap();
        assert!(find(&rows, "claude-zq-9").is_some());
        assert!(find(&rows, "claude-zq-8").is_none(), "bedrock 是转售渠道");
        assert!(find(&rows, "gpt-zq-8").is_none(), "openrouter 是转售渠道");
        assert!(find(&rows, "llama-zq-1").is_none(), "不在厂商名单里的前缀一律不收");
    }

    #[test]
    fn non_chat_modes_are_dropped() {
        let entry = |mode: Option<&str>| {
            let mut v = chat("openai", 1e-6, 1e-6);
            match mode {
                Some(m) => v["mode"] = json!(m),
                None => {
                    v.as_object_mut().unwrap().remove("mode");
                }
            }
            v
        };
        let rows = parse(&feed(json!({
            "gpt-zq-chat": entry(Some("chat")),
            "gpt-zq-resp": entry(Some("responses")),
            "gpt-zq-nomode": entry(None),
            "gpt-zq-embed": entry(Some("embedding")),
            "gpt-zq-image": entry(Some("image_generation")),
        })))
        .unwrap();
        for ok in ["gpt-zq-chat", "gpt-zq-resp", "gpt-zq-nomode"] {
            assert!(find(&rows, ok).is_some(), "{ok} 应当被收");
        }
        for bad in ["gpt-zq-embed", "gpt-zq-image"] {
            assert!(find(&rows, bad).is_none(), "{bad} 不是对话模型");
        }
    }

    /// 分档取门槛 ≤256K 的最贵一档，两种写法都认。注入判据：`TIER_CAP_TOKENS` 改成 300_000 → 变红。
    #[test]
    fn tiered_prices_take_the_top_tier_at_or_below_256k() {
        let rows = parse(&feed(json!({
            "gemini/gemini-zq-pro": {"litellm_provider": "gemini", "mode": "chat",
                "input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5,
                "input_cost_per_token_above_200k_tokens": 2.5e-6, "output_cost_per_token_above_200k_tokens": 1.5e-5},
            "gpt-zq-long": {"litellm_provider": "openai", "mode": "chat",
                "input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5,
                "input_cost_per_token_above_272k_tokens": 8e-6, "output_cost_per_token_above_272k_tokens": 4e-5},
            "dashscope/qwen-zq-plus": {"litellm_provider": "dashscope", "mode": "chat", "tiered_pricing": [
                {"range": [0, 256000], "input_cost_per_token": 4e-7, "output_cost_per_token": 1.2e-6},
                {"range": [256000, 1000000], "input_cost_per_token": 1.2e-6, "output_cost_per_token": 3.6e-6}]},
        })))
        .unwrap();
        assert!(close(find(&rows, "gemini-zq-pro").unwrap().input, 2.5), "200K 档 ≤256K → 取贵档");
        assert!(close(find(&rows, "gpt-zq-long").unwrap().input, 4.0), "272K 档 >256K → 不取");
        assert!(close(find(&rows, "qwen-zq-plus").unwrap().input, 1.2), "起点恰为 256K 的那档要取");
    }

    #[test]
    fn absurd_or_placeholder_prices_are_rejected() {
        let rows = parse(&feed(json!({
            "gpt-zq-negative": chat("openai", -1e-6, 1e-6),
            "gpt-zq-huge": chat("openai", 1e-6, 0.01),
            "gpt-zq-string": {"litellm_provider": "openai", "mode": "chat",
                "input_cost_per_token": "0.000001", "output_cost_per_token": 1e-6},
            "gpt-zq-zero": chat("openai", 0.0, 0.0),
            "gpt-zq-free-input": chat("openai", 0.0, 1e-6),
        })))
        .unwrap();
        for bad in ["gpt-zq-negative", "gpt-zq-huge", "gpt-zq-string", "gpt-zq-zero"] {
            assert!(find(&rows, bad).is_none(), "{bad} 不该被收");
        }
        assert!(find(&rows, "gpt-zq-free-input").is_some(), "只有输入免费是正经价");
    }

    /// 整份不像样就整份拒绝（调用方沿用上一份），而不是拿一张残表去替换。注入判据：`MIN_ROWS` 改成 0 → 变红。
    #[test]
    fn a_table_that_lost_its_shape_is_rejected_whole() {
        assert!(parse(b"<html>blocked</html>").is_err());
        assert!(parse(b"[]").is_err());
        let few = serde_json::to_vec(&json!({"gpt-zq-only": chat("openai", 1e-6, 1e-6)})).unwrap();
        assert!(parse(&few).unwrap_err().contains("少于"));
        assert!(parse(&feed(json!({}))).is_ok(), "对照组：够数就收");
    }

    #[test]
    fn zero_cache_prices_mean_missing_and_duplicates_keep_the_pricier() {
        let mut zero = chat("openai", 1e-6, 4e-6);
        zero["cache_read_input_token_cost"] = json!(0.0);
        zero["cache_creation_input_token_cost"] = json!(0.0);
        let rows = parse(&feed(json!({
            "gpt-zq-zero-cache": zero,
            "gpt-zq-dup": chat("openai", 1e-6, 4e-6),
            "openai/gpt-zq-dup": chat("openai", 2e-6, 8e-6),
        })))
        .unwrap();
        let z = find(&rows, "gpt-zq-zero-cache").unwrap();
        assert_eq!((z.cache_read, z.cache_write), (None, None), "缓存价 0 = 没填，不是免费");
        assert!(close(find(&rows, "gpt-zq-dup").unwrap().input, 2.0), "同名多条留贵的");
    }

    #[test]
    fn missing_cache_prices_borrow_the_builtin_ratio() {
        // 内置 `gpt-4o`：命中 0.5×、不另收写入费（= 输入价）
        let p = priced("gpt-4o-2099-01-01", 4.0, 16.0).pricing;
        assert_eq!((p.input_per_ntx, p.cache_read_per_ntx, p.cache_creation_per_ntx), (4_000, 2_000, 4_000));
        assert!(best_builtin(&normalize("zq-synaroute-xk")).is_none(), "夹具前提：内置表认不出这个名字");
        let p = priced("zq-synaroute-xk", 3.0, 9.0).pricing;
        assert_eq!((p.cache_read_per_ntx, p.cache_creation_per_ntx), (3_000, 3_000), "认不出就按输入价（偏高）");
        let explicit = OnlineRow {
            frag: "gpt-4o-2099-01-01".into(),
            input: 4.0,
            output: 16.0,
            cache_read: Some(0.4),
            cache_write: Some(5.0),
        };
        let p = resolve(&explicit).pricing;
        assert_eq!((p.cache_read_per_ntx, p.cache_creation_per_ntx), (400, 5_000), "上游给了就用上游的");
    }

    /// 合并规则的核心。注入判据：把 `lookup_in` 的 `>` 改成 `>=`（平局在线赢）→ 第一条断言变红。
    #[test]
    fn builtin_wins_ties_and_online_wins_only_when_more_specific() {
        let online = vec![
            priced("gpt-4o", 9.0, 9.0),
            priced("gpt-4o-2024-05-13", 5.0, 15.0),
            priced("gpt-6-astra-mini", 1.0, 4.0),
        ];
        let input_of = |m: &str, table: &[Priced]| lookup_in(m, table).unwrap().pricing.input_per_ntx;
        assert_eq!(input_of("gpt-4o", &online), 2_500, "同名平局必须内置赢");
        assert_eq!(input_of("gpt-4o-2024-05-13", &online), 5_000, "更具体的日期快照在线赢");
        assert_eq!(input_of("gpt-6-astra-mini", &[]), 10_000, "对照组：没有在线表时被钉在父型号上");
        assert_eq!(input_of("gpt-6-astra-mini", &online), 1_000);
        assert_eq!(input_of("openai/gpt-4o-2024-05-13", &online), 5_000, "中转前缀照样认得");
        assert_eq!(input_of("gpt-4o-2024.05.13", &online), 5_000, "小数点写法照样认得");
    }

    /// 在线片段只能从词首命中。注入判据：`lookup_in` 里 `match_at(.., true)` 改成 `false` → 变红。
    #[test]
    fn online_fragments_must_start_at_a_word() {
        let online = vec![priced("zq-synaroute-9", 3.0, 3.0)];
        assert!(lookup_in("zq-synaroute-9", &online).is_some());
        assert!(lookup_in("acme-zq-synaroute-9", &online).is_some(), "紧跟 `-` 算词首");
        assert!(lookup_in("acmezq-synaroute-9", &online).is_none(), "从词中间开始不算");
    }

    /// 在线行同样守版本边界：`x-5` 不吃掉更晚的小版本 `x-5-6`，但认它的日期快照。
    #[test]
    fn online_rows_respect_the_minor_version_boundary() {
        let online = vec![priced("zq-synaroute-5", 3.0, 3.0)];
        assert!(lookup_in("zq-synaroute-5-6", &online).is_none());
        assert!(lookup_in("zq-synaroute-5-20260101", &online).is_some());
    }

    /// 在线命中一律是「精确」：界面不该给一个在线表认得的具体型号打 `≈`。
    #[test]
    fn online_hits_are_exact_never_family() {
        assert!(lookup_in("claude-opus-9-9", &[]).unwrap().is_family, "对照组：内置表只能按家族猜");
        let hit = lookup_in("claude-opus-9-9", &[priced("claude-opus-9-9", 7.0, 35.0)]).unwrap();
        assert!(!hit.is_family);
        assert_eq!(hit.pricing.input_per_ntx, 7_000);
    }

    #[test]
    fn disk_cache_round_trips_and_rejects_stale_rules() {
        let dir = std::env::temp_dir().join(format!("synaroute-pricing-online-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = file_path(&dir.join("config.json"));
        let rows = parse(&feed(json!({}))).unwrap();
        let d = OnDisk { version: FILE_VERSION, rules_version: RULES_VERSION, fetched_at_ms: 42, source: "cdn".into(), rows: rows.clone() };
        save(&path, &d);
        let back = load(&path).expect("刚写的缓存应当读得回来");
        assert_eq!((back.fetched_at_ms, back.rows), (42, rows));
        save(&path, &OnDisk { rules_version: RULES_VERSION + 1, ..d });
        assert!(load(&path).is_none(), "规则版本不同的缓存必须作废");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load(&path).is_none(), "坏文件当没有，不能崩");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本地服务：按路径回固定响应，每个连接答一次即关。
    async fn mock(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).to_string();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (status, body) = routes
                        .iter()
                        .find(|(p, _, _)| *p == path)
                        .map(|(_, st, b)| (*st, b.clone()))
                        .unwrap_or((404, Vec::new()));
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&body).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        base
    }

    /// 镜像按序回落：回 500 的、回 200 却解析不出的都要跳过；全部失败时每个镜像的原因都在。
    #[tokio::test]
    async fn mirrors_fall_through_until_one_parses() {
        let routes = vec![("/500", 500, Vec::new()), ("/junk", 200, b"<html>blocked</html>".to_vec()), ("/good", 200, feed(json!({})))];
        let base = mock(routes).await;
        let (u500, junk, good) = (format!("{base}/500"), format!("{base}/junk"), format!("{base}/good"));
        let (rows, source) = fetch_from(&[u500.as_str(), junk.as_str(), good.as_str()], MAX_BODY_BYTES).await.unwrap();
        assert_eq!((rows.len(), source.as_str()), (MIN_ROWS, "127.0.0.1"));
        let err = fetch_from(&[u500.as_str(), junk.as_str()], MAX_BODY_BYTES).await.unwrap_err();
        assert!(err.contains("HTTP 500") && err.contains("JSON"), "{err}");
    }

    /// 超过上限的响应体中途就停。注入判据：去掉 `fetch_bytes` 里的上限判断 → 变红。
    #[tokio::test]
    async fn oversized_bodies_are_rejected() {
        let body = feed(json!({}));
        let base = mock(vec![("/big", 200, body.clone())]).await;
        let url = format!("{base}/big");
        let err = fetch_from(&[url.as_str()], body.len() - 1).await.unwrap_err();
        assert!(err.contains("超过"), "{err}");
        assert!(fetch_from(&[url.as_str()], body.len()).await.is_ok(), "对照组：刚好等于上限时照收");
    }

    /// 生产查价必须真的读装上的在线表。上面的合并用例都直接给 `lookup_in` 传表 —— 把 `lookup`
    /// 里那行 `current()` 换成空表，它们照样全绿；本条就是为那条接线写的。
    #[test]
    fn estimate_cost_consults_the_installed_online_table() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_state();
        let one_m = TokenUsage { input: 1_000_000, ..Default::default() };
        let probe = "zq-synaroute-wiring-probe";
        let before = estimate_cost(&one_m, Some(probe), None).1;
        install(&[OnlineRow { frag: probe.into(), input: 7.0, output: 21.0, cache_read: None, cache_write: None }], 1, "test");
        let after = estimate_cost(&one_m, Some(probe), None);
        restore_state(saved);
        assert_eq!(before, PricingSource::Unknown, "夹具前提：内置表认不出这个名字");
        assert_eq!(after, (Some(7_000_000_000), PricingSource::Exact));
    }

    /// 被手改过的磁盘缓存里的非 ASCII 片段不许进匹配器：`match_at` 逐字节推进，遇到它会在字符
    /// 中间切片而 panic —— 那是在转发热路径上（预算闸门每请求查价）。
    /// 注入判据：去掉 `install` 里的 `is_ascii` 过滤 → 变红。
    #[test]
    fn non_ascii_fragments_never_reach_the_matcher() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_state();
        install(&[OnlineRow { frag: "模型-5".into(), input: 1.0, output: 1.0, cache_read: None, cache_write: None }], 1, "test");
        let kept = current().map_or(0, |s| s.rows.len());
        let priced = std::panic::catch_unwind(|| estimate_cost(&TokenUsage::default(), Some("模型-5-6-模型-5"), None));
        restore_state(saved);
        assert_eq!(kept, 0, "非 ASCII 片段应当在装载时就被丢掉");
        assert!(priced.is_ok(), "查价在非 ASCII 名字上 panic 了");
    }

    /// 刷新失败只记原因、保留上一份；成功会落盘并清掉失败原因。
    /// 注入判据：让 `apply` 的 `Err` 分支也清空 `table` → 变红。
    #[test]
    fn a_failed_refresh_keeps_the_previous_table() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_state();
        let dir = std::env::temp_dir().join(format!("synaroute-pricing-apply-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        let rows = parse(&feed(json!({}))).unwrap();
        apply(Ok((rows.clone(), "cdn.jsdelivr.net".into())), &cfg, 1_000);
        let ok = status();
        apply(Err("cdn.jsdelivr.net：HTTP 503".into()), &cfg, 2_000);
        let failed = status();
        let on_disk = load(&file_path(&cfg)).map(|d| d.rows);
        restore_state(saved);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((ok.online_rows, ok.online_error.as_deref()), (MIN_ROWS, None));
        assert_eq!(failed.online_rows, MIN_ROWS, "失败不许丢掉上一份");
        assert_eq!(failed.online_fetched_at_ms, Some(1_000), "失败不许假装刷新过");
        assert!(failed.online_error.as_deref().is_some_and(|e| e.contains("503")));
        assert_eq!(on_disk, Some(rows), "成功那一轮要落盘");
    }

    #[test]
    fn refresh_is_due_daily_and_after_clock_rollback() {
        let day = REFRESH_MS;
        assert!(is_due(None, 0), "没有表就该拉");
        assert!(!is_due(Some(1_000), 1_000 + day - 1));
        assert!(is_due(Some(1_000), 1_000 + day));
        assert!(is_due(Some(10 * day), day), "时间戳在未来（时钟回拨）也该拉，否则刷新停摆");
        assert!(is_due(Some(i64::MIN), 0), "离谱的时间戳不能让减法溢出");
    }

    /// 后台线程必须在 `lib.rs` 里起。上面的用例全都直接调函数，把那一行删掉它们照样全绿，
    /// 而表现是「在线表永远是空的」—— 没有任何报错。
    #[test]
    fn lib_rs_spawns_the_background_refresh() {
        let lib = crate::proxy::custom_headers::production_code_only(include_str!("../lib.rs"));
        assert!(lib.contains("pricing::online::spawn_background("), "lib.rs 里没有起在线单价的后台线程");
    }

    /// 用生产客户端真拉一次（`#[ignore]`，要联网）：解压、系统代理、真实体积、镜像顺序都走真的。
    /// `SYNAROUTE_PRICING_FETCH_PROBE=1 cargo test --lib real_mirrors -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_mirrors_return_a_usable_table() {
        if std::env::var("SYNAROUTE_PRICING_FETCH_PROBE").is_err() {
            return;
        }
        let started = std::time::Instant::now();
        let (rows, source) = fetch_from(MIRRORS, MAX_BODY_BYTES).await.expect("所有镜像都失败了");
        println!("{} rows from {source} in {:?}", rows.len(), started.elapsed());
        assert!(rows.len() >= MIN_ROWS);
    }

    /// 拿真实的 LiteLLM 快照核一遍筛选规则（`#[ignore]`，要先自己下载那份 JSON）：
    /// `SYNAROUTE_LITELLM_PROBE=<路径> cargo test --lib real_litellm -- --ignored --nocapture`。
    /// 打印与内置表同名型号的一致率；2026-10-13 那份 JS 原型实测为共有 108、一致 101。
    #[test]
    #[ignore]
    fn real_litellm_snapshot_agrees_with_the_builtin_table() {
        let Ok(path) = std::env::var("SYNAROUTE_LITELLM_PROBE") else { return };
        let rows = parse(&std::fs::read(path).unwrap()).unwrap();
        let near = |a: u64, b: u64| (a as f64 - b as f64).abs() <= (b as f64 * 0.02).max(6.0);
        let (mut shared, mut agree) = (0, 0);
        for r in &rows {
            let builtin = super::super::table::PRICING_TABLE.iter().find(|b| b.frag == r.frag && !super::super::is_family_row(b.frag));
            let Some(b) = builtin else { continue };
            shared += 1;
            let (o, bi) = (resolve(r).pricing, ModelPricing::from_row(b));
            if near(o.input_per_ntx, bi.input_per_ntx) && near(o.output_per_ntx, bi.output_per_ntx) && near(o.cache_read_per_ntx, bi.cache_read_per_ntx) {
                agree += 1;
            } else {
                println!("{:<28} online {:?} builtin {:?}", r.frag, o, bi);
            }
        }
        println!("accepted {} rows; shared with the built-in table {shared}; agree {agree}", rows.len());
        assert!(rows.len() >= MIN_ROWS);
    }
}
