//! 模型池：**对外能选哪些模型**（并集口径）与**这次请求该优先走谁**（模型感知路由）。
//!
//! # 🔴 为什么这两件事必须在同一个模块、同一次上线
//!
//! 它们是同一条判据的两侧，拆开任何一半都比改之前糟：
//!
//! | 只改这一半 | 后果 |
//! |---|---|
//! | 对外列表改并集、排序不动 | 用户选 `glm-4.6`（只有备用 Key B 有）→ 候选按 `priority` 排、主 Key A 第一 → A 上 `resolve_model` 落到 A 的 `default_model` → **第一跳就被静默换成别的模型，压根没等到故障转移**。而并集里绝大多数模型都只有部分 Key 支持，这是常态、不是边缘情况 |
//! | 排序改了、列表仍取交集 | 排序永远无事可做（交集里每条 Key 都原生支持每个名字），等于白改 |
//!
//! # 三条判据
//!
//! 1. [`confidence`] —— 「这条 Key 对这个名字有多少把握」（三态：确定认识 / 不知道 / 确定会换掉）。
//! 2. [`discoverable_models`] —— 对外列表 = 各 Key 可服务集的**并集**（曾是交集）。
//! 3. [`rank_candidates`] —— 候选排序把「服务把握」摆在 `priority` **之前**。
//!
//! 加上 [`reject_if_unserviceable`]：我们宣称过的名字，全池临时服务不了时**响亮地报错**，
//! 而不是悄悄换一个模型糊过去。
//!
//! # 交集 → 并集：三个场景下都不比交集差
//!
//! | 多 Key 的对外名 | 交集（旧） | 并集（现在） |
//! |---|---|---|
//! | 完全一致 | = 全集 | = 全集，**无变化** |
//! | 部分重合 | 只剩共有的那几个 | 全部可见，各自路由到支持者 |
//! | 完全不重合 | 交集空 → 回退主 Key，备用 Key 的模型**根本看不到** | 全部可见 |
//!
//! 故**不设开关**：没有用户因此变差，而多一个开关就多一处可能静默失效的配置。
//!
//! 旧口径的代价从来不是零 —— 交集为空时它回退主 Key 超集，`codex_catalog` 里那句
//! 「目录里可能有备用 Key 服务不了的条目，故障转移到那条时会 404」记的就是这条路。
//! 两种失效形态都真实存在：备用 Key 有 `models` → 静默换模型；备用 Key 只配了映射
//! （`models` 为空）→ `resolve_model` 落到透传 → 上游 404。模型感知路由同时消掉这两种。
//!
//! # 与其它两层弹性的分工
//!
//! Key 级熔断、单模型锁定管的是「**这条 Key 现在能不能用**」；本模块管的是
//! 「**哪条 Key 认识这个模型**」。前者是运行态、由失败驱动；后者是配置态、恒定成立。
//! 三者在 [`rank_candidates`] 里叠加：配置态决定顺序，运行态决定去留。

use super::{error_resp_with_retry_after, ResBody};
use crate::model::{CategoryType, ModelResolveKind, ProviderKey};
use crate::store::Store;
use hyper::{Response, StatusCode};

/// 一条 Key 对某个对外模型名的「服务把握」。**变体顺序就是优先顺序**（`Ord` 从上到下递增），
/// [`rank_candidates`] 直接把它当排序键的第一位。
///
/// # 🔴 为什么必须是三态，不能是「支持 / 不支持」两态
///
/// 「不知道」和「确定不是」的处置完全不同，而混成一态的方向是**误伤**：
/// 一条**压根没配模型信息**的 Key（新加的直连 Key、只填了映射还没拉列表）会把请求
/// **原样透传**给上游，上游很可能认识那个名字。把它判成「不支持」会让我们对一个本来
/// 会成功的请求直接回 503。
///
/// 判据与 `balance_gate` 的「**查不到 ≠ 为零**」完全同源 —— 那里也是三态
/// （`Unknown`/`Ok`/`Exhausted`），理由一字不差。
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
enum Confidence {
    /// 映射 / 三档 / 原生同名命中 —— **确定**认识它，请求会照原样打过去。
    Native,
    /// 该 Key 没有任何模型信息，`resolve_model` 会原样透传 —— 上游认不认识**我们不知道**。
    Unknown,
    /// 该 Key 有模型信息、而请求的名字不在其中 → 会被换成它的 `default_model` 或列表首个，
    /// **确定**不是用户要的那个模型。
    Fallback,
}

/// 这条 Key 对这个对外名的把握。
///
/// **空名一律 `Native`**：下游没给模型名时（Claude 桌面端的部分请求）不该把所有 Key 都
/// 判成可疑 —— 那会让排序整体失效、并让 [`reject_if_unserviceable`] 对一个正常请求误报。
///
/// 刻意写成穷举 `match`：新增 [`ModelResolveKind`] 变体时编译器会逼人回答「这条新路径
/// 属于哪一态」，而判错的方向是静默的。
fn confidence(key: &ProviderKey, outward: &str) -> Confidence {
    if outward.trim().is_empty() {
        return Confidence::Native;
    }
    match key.resolve_model_detail(outward).1 {
        ModelResolveKind::Mapping | ModelResolveKind::Tier | ModelResolveKind::Native => {
            Confidence::Native
        }
        ModelResolveKind::Passthrough => Confidence::Unknown,
        ModelResolveKind::Default | ModelResolveKind::First => Confidence::Fallback,
    }
}

/// 这条 Key **有可能**服务这个对外名（即：不会把它换成别的模型）。
///
/// 也就是 `Native` 或 `Unknown`。用于**路由侧**的判定（排序、以及「要不要报 503」）——
/// 那里「不知道」的 Key 值得一试。**能力断言**（Codex 目录的档位与窗口）要用更严的
/// [`serves_natively`]，理由见它的文档。
pub(crate) fn may_serve(key: &ProviderKey, outward: &str) -> bool {
    confidence(key, outward) != Confidence::Fallback
}

/// 这条 Key **确定认识**这个对外名（`Confidence::Native`）。
///
/// # 与 [`may_serve`] 的分工：路由 vs 能力断言
///
/// 路由要宽（`may_serve`）—— 「不知道」的 Key 值得一试，替上游拒绝是误伤。
/// 而**能力断言**要严（本函数）：Codex 目录里的「这个模型支持哪些思考档位、窗口多大」
/// 是我们对用户做的承诺，一条没配模型信息、只会原样透传的 Key 对此**没有任何依据**。
///
/// 🔴 用错方向的代价是实测过的：2026-08-31 用户 17 条 Key 里只要有**一条**空配置的
/// Chat 协议 Key，`may_serve` 就让它成为**每个**模型的 owner → 交集判定当场把
/// **全部**模型的档位声明抹掉 → Codex 里所有模型的推理强度选择器一起消失。
/// 那与改动前「全池取交集」一样糟，等于这一维白改。
///
/// 按 Native 判是有依据的、不是「碰巧大多数时候对」：[`rank_candidates`] **保证**请求
/// 优先落到 Native 的 Key 上，Unknown 只在 Native 全被运行态挡住时才接手 —— 那时档位
/// 跟着降级是可接受的（而拿不到回答才是真问题）。
pub(crate) fn serves_natively(key: &ProviderKey, outward: &str) -> bool {
    confidence(key, outward) == Confidence::Native
}

/// 对外可选模型集：各 Key [`ProviderKey::serviceable_models`] 的**并集**，去重、保序。
///
/// `candidates` 必须已按 `priority` 升序（调用方一律传 `Store::enabled_keys_sorted`）。
///
/// # 🔴 顺序是契约，不只是观感
///
/// 三处消费者都只看**首个**：Claude CLI 接入把它写进 `env.ANTHROPIC_MODEL` + 顶层 `model`；
/// Codex 模型目录按 index 递增算 `priority` 并据此挑默认模型；桌面端选择器的默认项也是它。
/// 故必须是「主 Key 的全部（保其自身顺序）→ 再按 priority 依次追加各备用 Key 独有的」。
///
/// # 为什么不再取交集
///
/// 见模块头那张表。一句话：交集把「备用 Key 独有的模型」整个藏了起来，而它们明明可用 ——
/// 只要请求真的被路由到那条 Key（这正是 [`rank_candidates`] 保证的事）。
///
/// # 与 [`confidence`] 的口径差异（有意为之）
///
/// `serviceable_models` 有映射时**只暴露 `expected_name`**（刻意隐藏上游真实名，
/// 免得「对外名 + 真实名」双暴露）；而 `confidence` 会认真实名
/// （`resolve_model_detail` 第 3 级 `Native`）。故 **本函数的结果 ⊆ 有 Key 判为 `Native`
/// 的名字集合** —— 方向是「凡我们宣称的，一定有 Key 真的认识」，反之不成立（用户手打一个
/// 我们没宣称的真实名也能被正确路由）。[`reject_if_unserviceable`] 依赖的正是这个方向。
pub(crate) fn discoverable_models(candidates: &[ProviderKey]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for key in candidates {
        for name in key.serviceable_models() {
            // HashSet 去重而不是 `out.iter().any(..)`：并集口径下 m 可达上百
            // （6 条 Key × 各 30 个模型），线性扫描是 O(m²)。一次 String clone 换掉它。
            if seen.insert(name.clone()) {
                out.push(name);
            }
        }
    }
    out
}

/// 用户可见文案里最多列几个模型名。
///
/// 并集口径下 `discoverable_models` 可达上百条，而同一份文本要进三个地方：HTTP 响应体
/// （客户端会显示给用户）、事件环 detail、以及随之进 `logs/*.jsonl` 与诊断报告最近 200 条。
/// 本仓对上游与日志体积一贯设上限（`REQ_LOG_CAP` / `TAIL_WINDOW_BYTES` / `log_rotate`），
/// 这里同理 —— 一屏模型名对排障零价值，而它会随用户配置线性膨胀。
const MAX_LISTED_MODELS: usize = 8;

/// 同一个 (分类, 模型) 多久之内不再落第二条「当前不可用」事件。
///
/// # 🔴 折叠救不了这里 —— 本仓第二次踩同一个坑
///
/// `append_event_collapsible` 只合并**紧邻**的上一条。而客户端收到 503 会自动重发，
/// 两次重发之间还夹着别的 failover / error 事件 —— 折叠链一断就各占一行。更根本的是
/// 文案里带「预计 Ns 后恢复」这个**每秒都在变**的数字，即使紧邻也折不进去。
/// `balance_gate` 的注释里写着同一句话（「折叠救不了这里」），我写这个模块时没应用它。
///
/// **实测代价**（用户 2026-08-31 的日志，v0.1.44）：6 分钟 **88 条**、三个模型各 30 条、
/// `repeat` 全是 1，占满 `MAX_EVENTS`(500) 的 17% —— 而排障最需要的那几条 failover
/// 正被它们挤出环。窗口取 60s，与 Key 级熔断窗口同量级：一个熔断周期只该说一次。
const ANNOUNCE_THROTTLE_MS: i64 = 60_000;

/// 这条「模型不可用」现在该不该落事件（进程级节流，见 [`ANNOUNCE_THROTTLE_MS`]）。
///
/// 键的组合数有界：调用方的判据保证只有**我们宣称过的**名字才走到这里
/// （并集清单 ∪ 应用内选定的那一个），故上限 = 分类数 ×（并集大小 + 1）。
fn should_announce(category: CategoryType, model: &str, now: i64) -> bool {
    static SEEN: std::sync::Mutex<Option<std::collections::HashMap<String, i64>>> =
        std::sync::Mutex::new(None);
    // 锁中毒也要继续（这只是节流，panic 掉它不如放过一条事件）。
    let mut guard = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = guard.get_or_insert_with(std::collections::HashMap::new);
    let key = format!("{}:{model}", category.as_str());
    match seen.get(&key) {
        Some(&last) if now - last < ANNOUNCE_THROTTLE_MS => false,
        _ => {
            seen.insert(key, now);
            true
        }
    }
}

/// 一次**静默降级**留痕：请求成功了，但用的不是用户要的那个模型。
///
/// # 🔴 为什么必须单独一条 warning，而不是指望那行 route 日志
///
/// 用户 2026-09-01 实报的现场：他在 Codex 里选 `grok-4.6`，唯一支持它的 luckyg 被自己的
/// 网关连续 400，于是转移到 agentrouter → 那条 Key 不支持 grok-4.6 → 兜底改写为它的
/// `default_model`（`glm-5.3`）→ **流式返回 200**。日志里 31 轮全是这个形态。
///
/// 客户端那边看起来完全正常，而 route 那行是**绿色的「成功」**，「兜底改写为 glm-5.3」
/// 只是它模型段里的一小句 —— 没人会去逐行读成功日志。于是用户以为自己在用 grok，
/// 拿到的是 glm 的回答。
///
/// `reject_if_unserviceable` 拦不住这一支：它只在**进入候选循环之前**判一次，那一刻 luckyg
/// 还没失败、`may_serve` 为真、放行是对的。**降级发生在第二跳**，而那时没有任何人再判一次。
///
/// # 只对「我们宣称过的名字」留痕，而那有**两个**来源
///
/// 主判据与 503 闸门同源（并集清单）。客户端自己编的名字（CC 的带日期后缀家族名、Codex
/// 未重启时的内置 GPT 名）本来就该被三档/`default_model` 改写 —— 那是那两个机制存在的
/// 全部理由，对它们留痕会把日志刷成噪音。
///
/// 🔴 **但 `active_models` 里那个名字也必须算**，哪怕它此刻不在并集清单里。它是用户在**我们
/// 自己的界面**上选的，本就不可能是「客户端编的」。漏掉它的失效场景是真实发生过的
/// （2026-09-01 用户实报）：那一刻 luckyg 的模型列表还没拉到 `grok-4.5`，于是它在
/// `active_models` 里、却不在并集里 —— 每次请求都被兜底改写成 `glm-5.3`，而这道门把唯一
/// 一条线索也挡掉了。
///
/// v0.1.48 曾用另一种修法（那名字没人能服务时**自动清掉**用户的选择），已于 v0.1.50 撤销：
/// 它的立论建立在读错的一份配置上，且会在「Key 的模型列表还没拉到」这种正常过渡期里
/// 替用户改设置 —— 而留痕已经足够，不必动他的配置。
///
/// # 🔴 与 [`reject_if_unserviceable`] 判据②的不对称是**刻意的**，别去「统一口径」
///
/// 那道门只认并集清单，本函数额外认 `active_models`。本仓的纪律通常是「口径必须同源」，
/// 故这处分叉必须写明白 —— 而**统一的方向只可能错**：
///
/// - 往 reject 那边统一（让它也认 `active_models`）→「选了个模型列表还没拉到的名字」
///   从「降级 + 一条红字」变成**每个请求都 503**，用户什么都做不了。远比降级糟。
/// - 往本函数这边统一（只认并集）→ 就是 v0.1.49 那个缺陷本身：用户实报的现场里
///   唯一一条线索被这道门挡掉，整件事完全静默。
///
/// 两者判的**不是同一件事**：那道门决定「要不要让请求失败」（宁窄勿宽，误伤代价是全废），
/// 本函数决定「要不要留一句话」（宁宽勿窄，代价只是多一行日志，而且有 60s 节流）。
///
/// 节流复用 [`should_announce`]（60s / 每个模型）：客户端会自动重发，实测 31 轮里
/// 每轮都会走到这里。
pub(crate) fn note_silent_downgrade(
    store: &Store,
    category: CategoryType,
    requested: &str,
    key: &ProviderKey,
    real: &str,
) {
    let bare = crate::model::unwrap_gateway_model_id(requested);
    if bare.is_empty() || bare == real {
        return;
    }
    // 只在「确定被换掉」时留痕。`Unknown`（原样透传）不算降级 —— 上游很可能认识它。
    if confidence(key, requested) != Confidence::Fallback {
        return;
    }
    let enabled = store.enabled_keys_sorted(category);
    let we_advertised = discoverable_models(&enabled).iter().any(|m| m == bare)
        || store.active_model_of(category).as_deref() == Some(bare);
    if !we_advertised {
        return;
    }
    let now = chrono::Utc::now().timestamp_millis();
    if !should_announce(category, &format!("downgrade:{bare}"), now) {
        return;
    }
    // 谁本来能服务它 —— 这句是用户判断「该去修哪条 Key」的唯一线索。
    //
    // 🔴 两支的**处置完全不同**，不能合成一句：有支持者 = 它此刻挂了（等它恢复 / 查那条 Key）；
    // 没有支持者 = 这个名字压根没人能服务（模型列表还没拉到，或那条 Key 被停用/删了）。
    // 后一支在判据放宽之后是**主路径**（`active_models` 里的陈旧选择），不是边缘情形。
    let owners: Vec<&str> = enabled
        .iter()
        .filter(|k| may_serve(k, requested))
        .map(|k| k.name.as_str())
        .collect();
    let why = if owners.is_empty() {
        // 🔴 **不许写「会自动拉取」** —— 全仓没有任何自动拉取模型列表的路径：落盘那条
        // `fetch_models` 命令前端零调用，唯一入口是 Key 编辑器里用户手点的「拉取」按钮
        // （走 `fetch_models_draft` + 保存）。说「等它自动拉」是把用户送去等一件永不发生的事，
        // 而这条事件是他手里唯一的线索。判据 `the_no_owner_hint_must_not_promise_auto_fetch`。
        "当前没有任何已启用 Key 能服务它 —— 常见成因是那条 Key 的模型列表还没拉到\
         （模型列表不会自动更新：打开那条 Key 点一下「拉取」再保存，或直接手填这个模型名），\
         也可能它已被停用/删除。列表就绪后可以重新选。"
            .to_string()
    } else {
        format!(
            "能服务它的是「{}」，那条 Key 此刻不可用（上游报错或已熔断），故障转移落到了这一条。",
            owners.join("、")
        )
    };
    store.append_event_collapsible(
        category,
        "warning",
        Some(&key.id),
        &format!(
            "⚠ 你要的是 {bare}，实际用的是 {real} —— 「{}」不支持 {bare}，已按它的兜底模型改写。\
             {why}回答来自 {real}，不是 {bare}。",
            key.name
        ),
        None,
        // 🔴 折叠键带 `key.id`：合并时 `push_event_in_memory` 会刷新 `detail` 却**保留第一条的
        // `key_id`/`key_name`**，而 detail 里点名的是「哪条 Key 不支持它」→ 徽标写 A、正文说 B。
        //
        // 今天**这条路不可达**（节流键不含 Key，60s 内落不了第二条；60s 后中间必已夹一条 route
        // 成功事件，而折叠只合并紧邻的上一条）—— 也就是说这里摘掉的是**隐患不是活缺陷**，
        // 改节流键或事件顺序的人才会撞上它。不影响事件频率。判据见测试段同名那条。
        Some(format!("downgrade:{}:{}:{bare}", category.as_str(), key.id)),
    );
}

/// 这个候选会不会把用户**明确点名**的模型悄悄换掉？是则本次请求不该用它。
///
/// # 🔴 为什么 [`note_silent_downgrade`] 不够（2026-09-01 用户实报的第二半）
///
/// 那个函数把降级**记下来**了，但降级照旧发生：用户在 Codex 里选 `grok-4.6`，luckyg 的网关
/// 因一个工具 schema 连续 400（`tool parameter root must be an object type` —— 与 Key 无关、
/// 换谁都一样），转移到 agentrouter → 它不支持 grok-4.6 → 兜底改写成 `glm-5.3` → **200**。
/// 客户端拿到的是 glm 的回答，而它以为在用 grok。留痕只是让这件事**可查**，没让它**不发生**。
///
/// v0.1.44 定过判据「宁可 503 也不悄悄换模型」，但那道门（[`reject_if_unserviceable`]）
/// 只在**进入候选循环之前**判一次：那一刻 luckyg 还没失败、`may_serve` 为真，放行是对的。
/// 本函数补的正是**第二跳**那次判定。
///
/// # 只拦「用户点名过」的，且判据与那道门同源
///
/// 名字必须来自并集清单或 `active_models`（= 我们自己界面上选的）。客户端自动发的家族名
/// （CC 的带日期后缀名、Codex 未重启时的内置 GPT 名）**必须放行** —— 被三档/`default_model`
/// 改写是那两个机制存在的全部理由，拦下它们就是把 CC 的杂活调度整个打死。
/// 这一点与 `note_silent_downgrade` 的口径刻意一致（连 `active_models` 那一支也一样），
/// 两者判的是同一件事的两半：一个决定「要不要留一句话」，一个决定「要不要用这条候选」。
///
/// # 拦下之后不是失败，是**继续找下一个**
///
/// 调用方 `continue` 到下一个候选。全部候选都会降级时循环自然走到尾部，
/// 由既有的全失败分支给出 503 —— 那正是「宁可报错也不悄悄换」想要的结果，
/// 而且沿用现成的错误路径（含 `Retry-After`），本函数一行都不用管。
pub(crate) fn would_silently_substitute(
    store: &Store,
    category: CategoryType,
    requested: &str,
    key: &ProviderKey,
) -> bool {
    let bare = crate::model::unwrap_gateway_model_id(requested);
    if bare.is_empty() {
        return false;
    }
    // `Unknown`（原样透传）不算降级：上游很可能认识它，拦下反而误伤。
    // 与 `note_silent_downgrade` 用同一个三态判据，别在这里退化成两态。
    if confidence(key, requested) != Confidence::Fallback {
        return false;
    }
    // 解析结果与请求名一致时没有替换发生（理论上 Fallback 下不会，判一次不吃亏）。
    if key.resolve_model(requested) == bare {
        return false;
    }
    let enabled = store.enabled_keys_sorted(category);
    discoverable_models(&enabled).iter().any(|m| m == bare)
        || store.active_model_of(category).as_deref() == Some(bare)
}

/// 从全部 Key 里挑出本次请求的候选，并排好序。返回 `(候选, 是否走了兜底)`。
///
/// 排序键 `(服务把握, 余额已耗尽, priority)` —— 三位都是「越靠前越该先用」，
/// [`Confidence`] 的变体顺序与 `bool: Ord`（`false` 在前）各自给出前两位。
///
/// # 🔴 「服务把握」摆在 `priority` 之前，是本模块的全部意义
///
/// 也就是说「主 Key 优先」的准确语义变成了「**在把握相同的前提下**主 Key 优先」。
/// 这是「用户选的模型要真的生效」的唯一实现方式：请求一个只有备用 Key 支持的模型时，
/// 就该直接走那条备用 Key，而不是先让主 Key 把它悄悄换成别的模型。
///
/// **主 Key 徽标 / 托盘 / 状态条不受影响** —— 那些读的是 `enabled_keys_sorted`
/// （「用户配置的优先级顺序」这一事实的来源）。运行态与配置态分开，判据与
/// `health::balance_gate` 完全一致：让徽标跟着每次请求跳的表现是「用户什么都没改，
/// 主 Key 自己换了一条」。有反向判据盯着这一点。
///
/// # 三层门槛在这里叠加
///
/// - **配置态**（本模块）：对这个模型有多少把握 → 决定**顺序**
/// - **运行态**（Key 级熔断 / 单模型锁定）：这条 Key 现在能不能用 → 决定**去留**
///
/// 兜底语义与 `health::select_candidates` 必须保持一致：全部被运行态挡住时忽略两层门槛、
/// 原样返回全部启用 Key（熔断本为「多 Key 快速切换」而设，无处可切时不应自杀成 503）。
/// **兜底也继承同一排序**（降级不剔除，同 `balance_gate`）：无处可切时，最有把握的那条
/// 仍该排在最前。该语义有多条测试锁住，改这里前先读那两处的文档。
pub(crate) fn rank_candidates(
    all: &[ProviderKey],
    category: CategoryType,
    requested_model: &str,
) -> (Vec<ProviderKey>, bool) {
    // 先按引用收集（零克隆），排序只动指针。
    let mut enabled: Vec<&ProviderKey> =
        all.iter().filter(|k| k.category_id == category && k.enabled).collect();
    // 🔴 必须是 `sort_by_cached_key` 而不是 `sort_by_key`：后者在比较过程中**反复**调用
    // key 函数（O(n log n) 量级），而 `confidence` 里那次 `resolve_model_detail` 带一次
    // `to_ascii_lowercase` 分配 + 若干字符串比较。这是**转发热路径**上每请求一次的地方，
    // 本仓整治过同类开销（「每请求克隆整份 AppSettings 3~4 次」）。
    // cached 版对每个元素只算一次，代价是一个 n 长的临时 Vec —— n 是该分类的启用 Key 数。
    enabled.sort_by_cached_key(|k| {
        (
            confidence(k, requested_model),
            crate::health::balance_gate::is_exhausted(k),
            k.priority,
        )
    });

    // 未熔断、且本次要的模型没被锁的优先。`is_candidate_for_model` 是纯函数
    // （只读 HealthState + 当前时间），调用方持着读锁时调用安全。
    let primary: Vec<ProviderKey> = enabled
        .iter()
        .filter(|k| {
            let real = if requested_model.is_empty() {
                None
            } else {
                Some(k.resolve_model(requested_model))
            };
            crate::health::is_candidate_for_model(&k.health, real.as_deref())
        })
        .map(|k| (*k).clone())
        .collect();
    if !primary.is_empty() {
        return (primary, false);
    }
    // 全部被运行态挡住 → 兜底：忽略门槛全部纳入。此时才克隆全部（罕见路径）。
    let used_fallback = !enabled.is_empty();
    (enabled.into_iter().cloned().collect(), used_fallback)
}

/// 我们宣称过的模型名，全池临时服务不了时 → 响亮地报错，而不是悄悄换一个模型。
///
/// 返回 `Some(响应)` 表示本次请求应当就此结束。
///
/// # 🔴 判据是「**我们宣称过这个名字吗**」，不是「客户端是哪一类」
///
/// - **宣称过**（在 [`discoverable_models`] 里）→ 那是用户从**我们给的清单**里选的
///   （Codex 菜单 / 桌面端选择器 / `claude` 的 `/model`，三份清单都由我们写入）。
///   全池现在服务不了它，就是我们说了做不到的事 → **报错**。
/// - **没宣称过** → 客户端自己编的名字：Claude Code 按任务发的
///   `claude-haiku-4-5-20241022`（带日期后缀，不在清单里）、Codex 还没重启时仍发的内置
///   `gpt-5.6-*`。对这些**照旧降级** —— 那正是三档改写与 `default_model` 存在的全部理由。
///
/// 为什么不按分类分支（Codex/桌面端报错、CLI 降级）：**Claude 桌面端是混合的** ——
/// 它既有模型选择器（用户选），又会自动发家族名干杂活（它的 `tier_rewrite` 是 `true`）。
/// 按分类判会让桌面端的杂活调度在「没配三档」时直接失败。而按「宣称过吗」判，
/// 同一条规则自动分清了这个混合体的两半。
///
/// # 什么时候真的会走到报错
///
/// 只在「可能服务该模型的 Key（`Native` 或 `Unknown`）全都被熔断 / 单模型锁挡住，而**其它**
/// Key 还活着」时。全部 Key 都被挡住时 [`rank_candidates`] 走忽略门槛的兜底，那些 Key 回到
/// 候选里、判据 ① 就返回了 —— 也就是说本函数**永远不会**让一个原本还能一试的请求变成失败。
pub(crate) fn reject_if_unserviceable(
    store: &Store,
    category: CategoryType,
    requested: &str,
    candidates: &[ProviderKey],
) -> Option<Response<ResBody>> {
    // ① 有候选**可能**服务它（`Native` 或 `Unknown`）→ 什么都不做。绝大多数请求走这条，
    // 故它排在最前：下面那次 `enabled_keys_sorted` 会克隆每条 ProviderKey
    // （含 models/mappings/health），而这里是转发热路径。
    //
    // 🔴 判据是 `may_serve` 而不是「确定认识」：一条压根没配模型信息的 Key 会把名字原样
    // 透传给上游，上游很可能认识它 —— 对那种情形回 503 是把「我们不知道」当成了
    // 「一定不行」，方向恰好是误伤（同 `balance_gate` 的「查不到 ≠ 为零」）。
    if candidates.iter().any(|k| may_serve(k, requested)) {
        return None;
    }
    // ② 我们从没宣称过这个名字 → 客户端自己编的，照旧降级。
    //
    // 🔴 **必须先剥网关别名前缀**（审查发现的缺陷）：`/v1/models` 对非 claude/anthropic
    // 前缀的名字包成 `claude-synaroute-<real>`（CLI 只展示这类 id），而
    // `resolve_model_detail` 的注释写着「选中后客户端发回别名」。不剥的话第三方中转的模型名
    // （glm / deepseek / kimi / gpt / grok… 几乎全是非 claude 前缀）在这里**永远认不出**
    // → 本闸门在 Claude CLI 分类上整体失效，而失效方向正是本模块要消除的那个：静默换模型。
    // 判据①不受影响（`confidence` → `resolve_model_detail` 内部已经剥过），所以这处漏剥
    // 是**静默**的，用不带前缀的名字写测试也抓不到。
    let bare = crate::model::unwrap_gateway_model_id(requested);
    let enabled = store.enabled_keys_sorted(category);
    if !discoverable_models(&enabled).iter().any(|m| m == bare) {
        return None;
    }
    // ③ 宣称过、却没有一条候选可能服务它。
    let supporters: Vec<&ProviderKey> =
        enabled.iter().filter(|k| may_serve(k, requested)).collect();
    let now = chrono::Utc::now().timestamp_millis();
    let retry_after = earliest_release_ms(&supporters, requested, now)
        .map(|until| ((until - now) as f64 / 1000.0).ceil() as i64);

    // 文案要**指对方向**：谁支持它、为什么现在不行、多久恢复、以及现在就能用什么。
    // 最后一项让用户（或客户端）能立刻换一个，不必去猜 —— 少了它，这条报错就只是
    // 「不行」，而用户手里明明有可用选项。
    //
    // `who` 不设上限：它的长度受启用 Key 数天然限制（用户不会配几十条）。而下面那个清单
    // **会**随配置线性膨胀，故必须截断。
    let who = supporters.iter().map(|k| k.name.as_str()).collect::<Vec<_>>().join("、");
    let when = match retry_after {
        Some(s) => format!("预计 {s}s 后恢复"),
        None => "恢复时间未知".to_string(),
    };
    let usable = discoverable_models(candidates);
    let alt = match usable.len() {
        0 => "当前没有其它可用模型".to_string(),
        n if n <= MAX_LISTED_MODELS => format!("现在可用的模型：{}", usable.join("、")),
        n => format!(
            "现在可用的模型（共 {n} 个，先列 {MAX_LISTED_MODELS} 个）：{}",
            usable[..MAX_LISTED_MODELS].join("、")
        ),
    };
    // 文案里用剥掉别名前缀的**真实对外名**：CLI 的选择器显示的就是它（`display_name`），
    // 而带前缀那个串用户从没见过。
    let msg = format!(
        "模型 {bare} 当前不可用：本池中能服务它的只有「{who}」，而它们现在都被熔断或\
         模型锁定挡住（{when}）。{alt}。SynaRoute 刻意不把它悄悄换成别的模型——\
         那会让你拿到一个并非所选模型的回答。"
    );

    // 落一条可折叠 warning：不落这层的话，用户只在客户端看到 503，而应用里毫无线索。
    // **但必须节流**：折叠只合并紧邻的一条，而这里两次之间会夹着 failover/error，
    // 且文案带每秒都在变的秒数 —— 实测 6 分钟刷了 88 条。见 `should_announce`。
    if should_announce(category, bare, now) {
        store.append_event_collapsible(
            category,
            "warning",
            None,
            &msg,
            None,
            Some(format!("model-unavailable:{}:{bare}", category.as_str())),
        );
    }

    // 503 而不是 404：模型并非不存在，是它的服务者**暂时**不可用；回 404 会让客户端
    // 认定该模型永久不存在、可能把它从自己的清单里划掉。也不用 529（那是「过载」）。
    Some(error_resp_with_retry_after(
        StatusCode::SERVICE_UNAVAILABLE,
        &msg,
        retry_after,
    ))
}

/// 支持该模型的那些 Key 中，**最早**有一条重新可用的时刻（毫秒时间戳）。
///
/// 单条 Key 的恢复时刻取「Key 级熔断」与「该模型的单模型锁」中**更晚**的那个
/// （两道门都得过），跨 Key 再取 `min`（任一条回来就够）。
///
/// 全都没有生效的门 → `None`。理论上不会发生（那样它就该在候选里了），
/// 此时不带 `Retry-After`：给一个凭空的秒数会让客户端按一个假期限退避。
fn earliest_release_ms(supporters: &[&ProviderKey], requested: &str, now: i64) -> Option<i64> {
    supporters
        .iter()
        .filter_map(|k| {
            let real = k.resolve_model(requested);
            let breaker = k.health.breaker_until.filter(|t| *t > now);
            let lock = k.health.model_locks.get(&real).map(|l| l.until).filter(|t| *t > now);
            match (breaker, lock) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            }
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HealthState, ModelInfo, ModelLock, ModelMapping};

    fn mi(name: &str) -> ModelInfo {
        ModelInfo {
            real_name: name.into(),
            source: "manual".into(),
            fetched_at: None,
            context_window: None,
            max_output_tokens: None,
        }
    }

    fn mapping(expected: &str, real: &str) -> ModelMapping {
        ModelMapping {
            id: format!("{expected}->{real}"),
            expected_name: expected.into(),
            real_name: real.into(),
        }
    }

    fn key(id: &str, priority: i32, models: &[&str]) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            name: format!("Key-{id}"),
            category_id: CategoryType::ClaudeCli,
            enabled: true,
            priority,
            models: models.iter().map(|m| mi(m)).collect(),
            ..Default::default()
        }
    }

    /// 把这条 Key 打成「Key 级熔断中」。
    fn breaker(mut k: ProviderKey, ms_from_now: i64) -> ProviderKey {
        k.health = HealthState {
            breaker_until: Some(chrono::Utc::now().timestamp_millis() + ms_from_now),
            ..Default::default()
        };
        k
    }

    /// 把这条 Key 上某个**真实**模型名打成「单模型锁定中」。
    fn model_lock(mut k: ProviderKey, real: &str, ms_from_now: i64) -> ProviderKey {
        let until = chrono::Utc::now().timestamp_millis() + ms_from_now;
        k.health.model_locks.insert(real.into(), ModelLock { until, fail_count: 3 });
        k
    }

    fn store_at(tag: &str) -> (Store, std::path::PathBuf) {
        // 进程内自增序号是必须的：本机 `timestamp_nanos` 量化粒度只有 100ns，同进程并发跑的
        // 几条用例会撞到同一个目录、互删对方文件（同 `ccswitch::db_copy_path` 那条）。
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sr_pool_{tag}_{}_{n}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        (store, dir)
    }

    // ---- confidence：三态判据 ----

    /// 三条**命中**路径（映射 / 三档 / 原生同名）都算「确定认识」。
    #[test]
    fn the_three_hit_paths_are_native() {
        let mut mapped = key("a", 0, &["glm-4.6"]);
        mapped.mappings = vec![mapping("claude-opus-4-8", "glm-4.6")];
        assert_eq!(confidence(&mapped, "claude-opus-4-8"), Confidence::Native, "映射命中");

        let mut tiered = key("b", 0, &[]);
        tiered.tier_opus = Some("deepseek-reasoner".into());
        assert_eq!(confidence(&tiered, "claude-opus-4-5"), Confidence::Native, "三档命中");

        assert_eq!(confidence(&key("c", 0, &["glm-4.6"]), "glm-4.6"), Confidence::Native);
    }

    /// 🔴 三态里最要紧的一条区分：`Passthrough` 是「**不知道**」，不是「不支持」。
    ///
    /// 一条没有模型信息的 Key 会把名字原样透传给上游，上游很可能认识它。把它并入
    /// `Fallback` 的后果被一条**既有**端到端用例当场抓住
    /// （`the_model_lock_is_keyed_by_the_upstream_name_not_the_client_facing_alias`）：
    /// 那种 Key 上一个本来会成功的请求会收到 503。判据同 `balance_gate` 的「查不到 ≠ 为零」。
    #[test]
    fn a_key_with_no_model_info_is_unknown_not_fallback() {
        assert_eq!(confidence(&key("a", 0, &[]), "gpt-5"), Confidence::Unknown, "完全空配置");

        let mut only_mapping = key("b", 0, &[]);
        only_mapping.mappings = vec![mapping("alias", "real")];
        assert_eq!(
            confidence(&only_mapping, "gpt-5"),
            Confidence::Unknown,
            "配了映射但没拉模型列表、且请求名没命中映射 → 仍是原样透传"
        );
    }

    /// 有模型信息、而请求的不在其中 → `Fallback`（**确定**会被换成别的模型）。
    #[test]
    fn a_key_that_would_swap_the_model_is_fallback() {
        let mut with_default = key("a", 0, &[]);
        with_default.default_model = Some("glm-4.6".into());
        assert_eq!(confidence(&with_default, "gpt-5"), Confidence::Fallback, "default_model");
        assert_eq!(confidence(&key("b", 0, &["glm-4.6"]), "gpt-5"), Confidence::Fallback, "列表首个");

        assert!(!may_serve(&with_default, "gpt-5"), "may_serve 只排除 Fallback 这一态");
    }

    /// 变体顺序即优先顺序 —— [`rank_candidates`] 直接把它当排序键第一位。
    /// 顺序反了不报错，只是路由悄悄变差（把「会换模型」的排到「确定认识」之前）。
    #[test]
    fn the_confidence_order_is_native_then_unknown_then_fallback() {
        assert!(Confidence::Native < Confidence::Unknown);
        assert!(Confidence::Unknown < Confidence::Fallback);
    }

    /// 空模型名一律 `Native`：下游没给名字时（桌面端的部分请求）不该把所有 Key 都判成可疑
    /// —— 那会让排序整体失效，并让 `reject_if_unserviceable` 对一个正常请求误报。
    #[test]
    fn an_empty_model_name_is_never_treated_as_unsupported() {
        let k = key("a", 0, &["glm"]);
        assert_eq!(confidence(&k, ""), Confidence::Native);
        assert_eq!(confidence(&k, "   "), Confidence::Native, "只有空白也算空");
        assert!(may_serve(&k, ""));
    }

    // ---- discoverable_models：并集 ----

    /// 主 Key 的全部在前、各备用 Key 独有的按 priority 追加、共有的只出现一次。
    ///
    /// 顺序是契约：首个会被写进 `env.ANTHROPIC_MODEL`、Codex 目录的默认模型、桌面端
    /// 选择器的默认项。断言写成完整的 `vec![..]` 而不是「包含」，就是为了钉住顺序。
    #[test]
    fn the_union_keeps_the_primary_first_and_appends_the_rest() {
        let a = key("a", 0, &["opus", "sonnet"]);
        let b = key("b", 1, &["sonnet", "glm"]); // sonnet 共有、glm 独有
        let c = key("c", 2, &["glm", "kimi"]); // glm 已在、kimi 独有
        assert_eq!(
            discoverable_models(&[a, b, c]),
            vec!["opus", "sonnet", "glm", "kimi"],
            "主 Key 全部在前，备用 Key 独有的按顺序追加，共有的只出现一次"
        );
    }

    /// 对外名完全不重合 —— 交集口径下备用 Key 的模型**根本看不到**（回退主 Key），
    /// 这正是用户「多 Key 时能选的模型太少」那个抱怨的核心场景。
    #[test]
    fn disjoint_keys_contribute_everything() {
        let a = key("a", 0, &["claude-opus-4-7"]);
        let b = key("b", 1, &["glm-4.6"]);
        assert_eq!(discoverable_models(&[a, b]), vec!["claude-opus-4-7", "glm-4.6"]);
    }

    /// 有映射时只暴露对外名（不并入真实名），与 `serviceable_models` 的口径一致 ——
    /// 否则用户配了 `opus-4-8 → glm-4.6` 之后，选择器里会同时冒出 `glm-4.6`。
    #[test]
    fn mappings_expose_only_the_outward_name() {
        let mut k = key("a", 0, &["glm-4.6", "glm-4.5"]);
        k.mappings = vec![mapping("opus-4-8", "glm-4.6")];
        assert_eq!(discoverable_models(&[k]), vec!["opus-4-8"]);
    }

    #[test]
    fn no_keys_means_no_models() {
        assert!(discoverable_models(&[]).is_empty());
    }

    // ---- rank_candidates：模型感知路由 ----

    fn ids(keys: &[ProviderKey]) -> Vec<&str> {
        keys.iter().map(|k| k.id.as_str()).collect()
    }

    /// 🔴 本模块的核心断言：认识这个模型的 Key 排在 `priority` 更小的那条**之前**。
    ///
    /// 没有这一条，「对外清单取并集」就等于「用户选备用 Key 独有的模型必被第一跳静默换掉」。
    #[test]
    fn a_key_that_knows_the_model_outranks_a_smaller_priority_number() {
        let a = key("a", 0, &["opus"]); // 主 Key，不认识 glm
        let b = key("b", 1, &["glm"]); // 备用 Key，认识 glm
        let (got, fb) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "glm");
        assert!(!fb, "两条 Key 运行态都好，不该是兜底路径");
        assert_eq!(ids(&got), vec!["b", "a"], "认识 glm 的 b 必须先试");
    }

    /// 都认识时回到纯 `priority` —— 「主 Key 优先」在同等条件下仍然成立。
    #[test]
    fn priority_still_decides_among_keys_that_all_know_the_model() {
        let a = key("a", 5, &["glm"]);
        let b = key("b", 1, &["glm"]);
        let (got, _) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "glm");
        assert_eq!(ids(&got), vec!["b", "a"]);
    }

    /// 「不知道」的 Key 排在「确定会换模型」的之前 —— 它至少有机会真的服务用户要的模型，
    /// 而后者一定不会。这是三态排序键的中间那一位在起作用。
    #[test]
    fn an_unknown_key_outranks_one_that_would_swap_the_model() {
        let swap = key("swap", 0, &["opus"]); // 有模型信息、不含 glm → Fallback
        let blank = key("blank", 1, &[]); // 无模型信息 → Unknown（原样透传）
        let (got, _) = rank_candidates(&[swap, blank], CategoryType::ClaudeCli, "glm");
        assert_eq!(ids(&got), vec!["blank", "swap"], "priority 更大的 blank 仍该先试");
    }

    /// 反过来：「确定认识」排在「不知道」之前。
    ///
    /// 少了这一条，把 [`Confidence`] 的 `Native` 与 `Unknown` 两个变体**互换声明顺序**
    /// 全套测试照样绿（注入实测）—— 而那意味着一条空配置 Key 会抢在真正认识该模型的
    /// Key 前面，白打一次上游、还可能拿到 404。
    #[test]
    fn a_key_that_knows_the_model_outranks_one_that_merely_might() {
        let blank = key("blank", 0, &[]); // Unknown
        let owner = key("owner", 1, &["glm"]); // Native
        let (got, _) = rank_candidates(&[blank, owner], CategoryType::ClaudeCli, "glm");
        assert_eq!(ids(&got), vec!["owner", "blank"], "确定认识的那条必须先试");
    }

    /// **降级不剔除**：一条都不认识时仍返回全部候选（按 priority），让 `resolve_model`
    /// 的兜底路径接管。硬剔除会让「配置不全」直接变成 503，而那比降级糟得多。
    ///
    /// 报不报错由 [`reject_if_unserviceable`] 按「我们宣称过吗」单独决定，不在这一层。
    #[test]
    fn nobody_knows_the_model_gives_plain_priority_order_not_an_empty_list() {
        let a = key("a", 0, &["opus"]);
        let b = key("b", 1, &["sonnet"]);
        let (got, fb) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "gpt-5");
        assert!(!fb, "运行态都好 → 不是兜底路径");
        assert_eq!(ids(&got), vec!["a", "b"]);
    }

    #[test]
    fn other_categories_and_disabled_keys_are_excluded() {
        let mut other = key("x", 0, &["glm"]);
        other.category_id = CategoryType::Codex;
        let mut off = key("y", 0, &["glm"]);
        off.enabled = false;
        let (got, _) =
            rank_candidates(&[other, off, key("a", 9, &["glm"])], CategoryType::ClaudeCli, "glm");
        assert_eq!(ids(&got), vec!["a"]);
    }

    /// 运行态压过配置态：熔断中的 Key 被剔除，**即使它是唯一认识该模型的那条**。
    /// 此时只剩会走兜底模型的 a —— 而那正是 [`reject_if_unserviceable`] 要拦下的情形。
    #[test]
    fn a_tripped_key_is_dropped_even_if_it_is_the_only_one_that_knows_the_model() {
        let a = key("a", 0, &["opus"]);
        let b = breaker(key("b", 1, &["glm"]), 60_000);
        let (got, fb) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "glm");
        assert!(!fb, "a 还活着 → 不是兜底路径");
        assert_eq!(ids(&got), vec!["a"]);
    }

    /// 全部被运行态挡住 → 兜底纳入全部，**且继承同一排序**：无处可切时，最可能认识这个
    /// 模型的那条仍该排最前（同 `balance_gate` 的「降级不剔除」）。
    #[test]
    fn the_fallback_path_still_puts_the_knowing_key_first() {
        let a = breaker(key("a", 0, &["opus"]), 60_000);
        let b = breaker(key("b", 1, &["glm"]), 60_000);
        let (got, fb) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "glm");
        assert!(fb, "全部熔断 → 兜底");
        assert_eq!(ids(&got), vec!["b", "a"]);
    }

    /// 单模型锁只挡那一个模型。锁的键是**上游真实名**，故换个模型名那条 Key 就回来了。
    #[test]
    fn a_model_lock_drops_the_key_for_that_model_only() {
        let a = key("a", 0, &["opus"]);
        let b = model_lock(key("b", 1, &["glm", "kimi"]), "glm", 60_000);
        let (got, _) = rank_candidates(&[a.clone(), b.clone()], CategoryType::ClaudeCli, "glm");
        assert_eq!(ids(&got), vec!["a"], "b 上的 glm 被锁");
        let (got, _) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "kimi");
        assert_eq!(ids(&got), vec!["b", "a"], "kimi 没被锁，且只有 b 认识它 → b 优先");
    }

    /// 🔴 三位排序键的**顺序**：模型匹配 > 余额未耗尽 > priority。
    ///
    /// 把余额摆在模型匹配之前的失效是：用户选了一个只有那条余额耗尽的 Key 支持的模型，
    /// 请求被送去一条压根不认识它的 Key，于是模型被静默换掉 —— 而余额闸门的语义只是
    /// 「降级」（`balance_gate` 明确写着不剔除），不该压过「谁认识这个模型」。
    #[test]
    fn knowing_the_model_outranks_a_healthy_balance() {
        use crate::model::{BalanceQuery, BalanceResult};
        let mut broke = key("broke", 9, &["glm"]);
        broke.balance_query = Some(BalanceQuery { enabled: true, ..Default::default() });
        broke.cached_balance =
            Some(BalanceResult { ok: true, remaining: Some(0.0), ..BalanceResult::failed("") });
        let rich = key("rich", 0, &["opus"]); // 余额未知（= 不算耗尽），但不认识 glm

        let (got, _) = rank_candidates(&[rich, broke], CategoryType::ClaudeCli, "glm");
        assert_eq!(
            ids(&got),
            vec!["broke", "rich"],
            "余额耗尽但认识 glm 的那条仍该先试 —— 余额只降级，不该改写「谁认识这个模型」"
        );
    }

    // ---- reject_if_unserviceable：宣称过的名字要么服务，要么响亮报错 ----

    /// 有候选认识它 → 什么都不做（绝大多数请求）。
    #[test]
    fn a_servable_model_is_never_rejected() {
        let (store, dir) = store_at("ok");
        let a = key("a", 0, &["glm"]);
        store.upsert_key(a.clone()).unwrap();
        assert!(
            reject_if_unserviceable(&store, CategoryType::ClaudeCli, "glm", &[a]).is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 我们**没宣称过**的名字一律放过 —— 那是客户端自己编的（Claude Code 按任务发的
    /// `claude-haiku-4-5-20241022` 带日期后缀、Codex 未重启时仍发内置 `gpt-5.6-*`），
    /// 对它们降级正是三档改写与 `default_model` 存在的全部理由。
    ///
    /// 判成报错会当场打断客户端的正常调度，而那是每次会话都在发生的事。
    #[test]
    fn a_name_we_never_advertised_is_passed_through_to_the_fallback_path() {
        let (store, dir) = store_at("unadvertised");
        let a = key("a", 0, &["glm"]);
        store.upsert_key(a.clone()).unwrap();
        assert!(
            reject_if_unserviceable(
                &store,
                CategoryType::ClaudeCli,
                "claude-haiku-4-5-20241022",
                &[a]
            )
            .is_none(),
            "清单里没有这个名字 → 它是客户端自己发的，照旧走兜底"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空模型名不该被拦（下游压根没给名字时，兜底本来就是为它设计的）。
    #[test]
    fn an_empty_model_name_is_never_rejected() {
        let (store, dir) = store_at("emptyname");
        let a = key("a", 0, &["glm"]);
        store.upsert_key(a.clone()).unwrap();
        assert!(reject_if_unserviceable(&store, CategoryType::ClaudeCli, "", &[a]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 回归：**三档一个都没配**时，Claude Code 的家族名请求绝不能被本闸门拦下。
    ///
    /// 这是本轮改动最坏的可能回归 —— CC 每次会话都按任务发 `claude-*-4-5-<日期>` 这类名字，
    /// 一旦它们变成 503，CC 直接完全不能用（而不是「某个模型不能用」）。
    ///
    /// 不会发生的原因在判据 ②：`serviceable_models` 只在**配了三档**时才追加
    /// `claude-{opus,sonnet,haiku}-4-5`，而且追加的是**不带日期后缀**的家族代表名。
    /// CC 实际发的带后缀名字压根不在我们宣称的清单里 → 一律放过，走 `resolve_model` 的兜底链
    /// （`default_model` → 列表首个 → 透传），也就是改动前的行为。
    #[test]
    fn claude_code_family_names_are_never_blocked_when_no_tier_is_configured() {
        let (store, dir) = store_at("cc_notier");
        let k = key("a", 0, &["glm-4.6", "glm-4.5-air"]); // 第三方中转：无三档、无映射
        store.upsert_key(k.clone()).unwrap();

        for m in ["claude-sonnet-4-5-20250929", "claude-opus-4-5", "claude-3-5-haiku-20241022"] {
            assert!(
                reject_if_unserviceable(&store, CategoryType::ClaudeCli, m, std::slice::from_ref(&k))
                    .is_none(),
                "{m} 不在我们宣称的清单里，必须放过去走兜底 —— 拦下它等于 Claude Code 全废"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 候选里有一条「没配模型信息」的 Key（会原样透传）→ **不报错**。
    ///
    /// 这条正是三态判据的存在理由。我第一版写成两态（把 `Passthrough` 并进「不支持」），
    /// 被一条**既有**端到端用例当场抓住：
    /// `the_model_lock_is_keyed_by_the_upstream_name_not_the_client_facing_alias` 里的 k2
    /// 就是这种空配置 Key，两态实现下那个本来 200 的请求变成了 503。
    #[test]
    fn a_passthrough_candidate_is_enough_to_avoid_rejecting() {
        let (store, dir) = store_at("passthrough");
        let blank = key("blank", 0, &[]); // 无模型信息 → Unknown
        store.upsert_key(blank.clone()).unwrap();
        let mut owner = key("owner", 1, &[]);
        owner.mappings = vec![mapping("alias-X", "up-X")];
        store.upsert_key(breaker(owner, 60_000)).unwrap();

        // alias-X 是我们宣称过的（由 owner 提供），而 owner 在熔断中 → 候选里只剩 blank。
        assert!(
            reject_if_unserviceable(&store, CategoryType::ClaudeCli, "alias-X", &[blank]).is_none(),
            "blank 会把 alias-X 原样透传、上游可能认识它 —— 不该替上游先拒绝"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 宣称过、却没有一条候选可能服务它 → 503 + Retry-After + 一条 warning 事件。
    ///
    /// 不报错的话用户会拿到「另一个模型的回答」，而客户端与日志都显示成功 —— 那是本仓
    /// 最忌讳的静默给错结果。文案的四要素（谁支持它 / 为什么不行 / 多久恢复 / 现在能用
    /// 什么）逐条断言：少了最后一项，用户明明手里有可用选项却只被告知「不行」。
    #[test]
    fn a_model_we_advertise_but_cannot_serve_right_now_is_rejected_loudly() {
        let (store, dir) = store_at("reject");
        let a = key("a", 0, &["opus"]);
        store.upsert_key(a.clone()).unwrap();
        store.upsert_key(breaker(key("b", 1, &["glm"]), 90_000)).unwrap();

        // 候选里只有 a —— b 在熔断中，已被 rank_candidates 剔除。
        let resp = reject_if_unserviceable(&store, CategoryType::ClaudeCli, "glm", &[a])
            .expect("宣称过 glm 却没有候选认识它 → 必须报错");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE, "503 而不是 404/529");
        let ra: i64 = resp
            .headers()
            .get("retry-after")
            .expect("必须带 Retry-After，否则客户端只会立刻重发")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((80..=90).contains(&ra), "Retry-After 要对齐熔断剩余时间，实得 {ra}");

        let ev = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .find(|e| e.detail.contains("glm"))
            .expect("必须留一条事件 —— 只回 503 的话应用里毫无线索");
        assert_eq!(ev.kind, "warning", "LogsPage 的分组是穷举的，别新造 kind");
        assert!(ev.detail.contains("Key-b"), "要说清是哪条 Key 支持它");
        assert!(ev.detail.contains("opus"), "要给出现在就能用的模型");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全部 Key 都被挡住时**不报错**：`rank_candidates` 那条兜底会把支持者放回候选里，
    /// 于是判据 ① 先返回。也就是说本函数永远不会把一个「原本还能一试」的请求变成失败。
    #[test]
    fn the_all_blocked_fallback_is_never_turned_into_a_rejection() {
        let (store, dir) = store_at("allblocked");
        let a = breaker(key("a", 0, &["opus"]), 60_000);
        let b = breaker(key("b", 1, &["glm"]), 60_000);
        store.upsert_key(a.clone()).unwrap();
        store.upsert_key(b.clone()).unwrap();

        let (cands, fb) = rank_candidates(&[a, b], CategoryType::ClaudeCli, "glm");
        assert!(fb, "前提：这一步走的是兜底");
        assert!(
            reject_if_unserviceable(&store, CategoryType::ClaudeCli, "glm", &cands).is_none(),
            "兜底把认识 glm 的那条放回来了 → 该让它去试，而不是直接失败"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 源码级接线判据 ----
    //
    // 上面**所有**行为用例都直接调本模块的函数，把接线改回去它们照样全绿 —— 而那正是
    // 缺陷本体。这是本仓第 14 次盯同一类盲区（前 13：mcp::handle_http / route_meta /
    // lan_guard 的 peer / log_rotate 的写线程 / custom_headers / model_choice::pick /
    // record_stream_end / stream_idle / thinking_rectify / sse_error / key_flags /
    // balance_gate×4）：**单元覆盖了组件 ≠ 覆盖了调用它的那条线**，且漏掉接线是静默的。

    /// 一律先剥注释再查。本仓已 5 次栽在「注释里的字面量满足了断言」上
    /// （`data-dir-env-name-must-match` / `userPrefsParity` /
    /// `only_v6_must_be_set_explicitly` / `check-tailwind-tokens` / 那个一次性扫查脚本）。
    fn prod(src: &str) -> String {
        crate::proxy::custom_headers::production_code_only(src)
    }

    /// `Store::candidates_for` 必须把决策交给 [`rank_candidates`]，不许自己再写一份排序。
    #[test]
    fn candidates_for_must_delegate_to_this_module() {
        let src = prod(include_str!("store.rs"));
        assert!(
            src.contains("model_pool::rank_candidates("),
            "candidates_for 必须调 rank_candidates —— 自己写一份排序必然与本模块漂移，\
             而漂移的表现是「用户选的模型时而生效时而不生效」"
        );
        assert!(
            !src.contains("balance_gate::is_exhausted(k), k.priority"),
            "store.rs 里不该再留旧的两位排序键 —— 那意味着模型维度被整个丢掉了"
        );
    }

    /// 🔴 转发路径必须调 [`reject_if_unserviceable`]，且**排在候选循环之前**。
    ///
    /// 只钉「调了」不够：挂进循环里的任何分支都意味着「至少已经打了一次上游、并已按那个
    /// 被换掉的模型出了结果」。同 `thinking_rectify` 那条钉位置而非只钉调用。
    #[test]
    fn the_forwarding_path_must_reject_before_the_candidate_loop() {
        let src = prod(include_str!("proxy.rs"));
        let call = src.find("model_pool::reject_if_unserviceable(").expect(
            "转发路径必须调 reject_if_unserviceable，否则我们宣称过的模型会被第一个候选静默换掉",
        );
        let loop_at = src
            .find("for (i, key) in candidates.iter().enumerate()")
            .expect("候选循环的形态变了，请同步本判据");
        assert!(call < loop_at, "必须排在候选循环之前（call={call} loop={loop_at}）");
        assert_eq!(
            src.matches("model_pool::reject_if_unserviceable(").count(),
            1,
            "只该有一处调用点"
        );
    }

    /// `proxy.rs` 生产段里不许长出第二份对外清单实现（并集或交集都不行）。
    #[test]
    fn proxy_must_not_grow_a_second_model_set_implementation() {
        let src = prod(include_str!("proxy.rs"));
        assert!(
            !src.contains("fn discoverable_models"),
            "对外清单只有 model_pool 一份实现，proxy.rs 只 re-export"
        );
        assert!(
            !src.contains("backup_sets"),
            "这是旧交集实现的形态（各备用 Key 建 HashSet 再取交集），别把它带回来"
        );
    }

    /// 🔴 **反向**判据：主 Key 徽标 / 托盘 / 状态条读的 `enabled_keys_sorted` 绝不能受
    /// 模型维度影响 —— 它是「用户配置的优先级顺序」这一事实的来源。
    ///
    /// 让它跟着每次请求的模型跳，表现是「用户什么都没改，主 Key 自己换了一条」。
    /// 与 `balance_gate` 对同一个函数的要求一字不差：运行态不改写配置视图。
    #[test]
    fn the_configured_order_view_must_not_be_model_aware() {
        let src = prod(include_str!("store.rs"));
        let at = src.find("pub fn enabled_keys_sorted").expect("函数改名了，请同步本判据");
        let end = src[at..].find("\n    }").map(|i| at + i).unwrap_or(src.len());
        let body = &src[at..end];
        assert!(
            !body.contains("may_serve") && !body.contains("confidence("),
            "enabled_keys_sorted 不许掺入模型维度"
        );
    }

    /// 🔴 回归（审查发现）：CLI 分类下客户端发回的是**网关别名**（`claude-synaroute-<real>`），
    /// 判据②必须先剥前缀才认得出「这个名字我们宣称过」。
    ///
    /// 不剥的表现：第三方中转的模型名（glm / deepseek / kimi / gpt / grok… 几乎全是非 claude
    /// 前缀）在这里永远认不出 → 本闸门在 Claude CLI 分类上**整体失效**，静默降级照旧发生。
    /// 判据①不受影响（`confidence` 内部已剥），所以漏剥是静默的、不带前缀的用例抓不到 ——
    /// 上面那条 `..._is_rejected_loudly` 用的 `glm` 恰好不带前缀，所以它一直是绿的。
    #[test]
    fn the_gateway_alias_prefix_must_be_stripped_before_the_advertised_check() {
        let (store, dir) = store_at("alias");
        let a = key("a", 0, &["opus"]);
        store.upsert_key(a.clone()).unwrap();
        store.upsert_key(breaker(key("b", 1, &["glm-4.6"]), 90_000)).unwrap();

        // 前提：CLI 的 /model 会把 glm-4.6 展示成带前缀的 id，选中后原样发回。
        let aliased = crate::model::to_gateway_model_id("glm-4.6");
        assert_eq!(aliased, "claude-synaroute-glm-4.6", "包装规则变了就该同步本用例");

        let resp = reject_if_unserviceable(
            &store,
            CategoryType::ClaudeCli,
            &aliased,
            std::slice::from_ref(&a),
        )
        .expect("别名形态也必须被认出来，否则闸门对 CLI 上的第三方模型全体失效");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 文案与折叠键都该用剥后的真实名 —— 带前缀那个串用户从没见过。
        let ev = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .find(|e| e.kind == "warning")
            .expect("必须留一条事件");
        assert!(ev.detail.contains("模型 glm-4.6"), "文案该用剥后的名字：{}", ev.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 可用模型清单必须截断：并集下它可达上百条，而同一份文本进响应体 + 事件 + 日志文件。
    #[test]
    fn the_usable_model_list_in_the_message_is_capped() {
        let (store, dir) = store_at("capped");
        let many: Vec<String> = (0..40).map(|i| format!("m{i}")).collect();
        let a = key("a", 0, &many.iter().map(String::as_str).collect::<Vec<_>>());
        store.upsert_key(a.clone()).unwrap();
        store.upsert_key(breaker(key("b", 1, &["only-b"]), 60_000)).unwrap();

        let resp =
            reject_if_unserviceable(&store, CategoryType::ClaudeCli, "only-b", &[a]).expect("应报错");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let ev = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .find(|e| e.kind == "warning")
            .expect("必须留一条事件");
        assert!(ev.detail.contains("共 40 个"), "要如实报总数：{}", ev.detail);
        assert!(!ev.detail.contains("m39"), "第 39 个不该被列出来（只列前 8 个）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 事件必须**按模型节流**，而 503 仍然每次都回。
    ///
    /// 两者的受众不同：客户端每次都需要那个 503（否则它以为请求成功了），而应用里的事件
    /// 只需要「这件事发生了」一次。不节流的实测代价（用户 2026-08-31 的日志，v0.1.44）：
    /// 6 分钟 **88 条**、三个模型各 30 条、`repeat` 全是 1，占满 `MAX_EVENTS`(500) 的 17%，
    /// 把排障最需要的那几条 failover 挤出环。
    ///
    /// 折叠指望不上：`append_event_collapsible` 只合并**紧邻**的一条，而两次重发之间夹着
    /// failover / error 事件，且文案带每秒都在变的「预计 Ns 后恢复」。
    ///
    /// ⚠️ 本用例用独有的模型名（`throttle-probe`）：节流表是**进程级** static，
    /// 与别的用例共用同一个键会互相污染（同 CLAUDE.md 里 `DENIED_TOTAL` 那条 flaky）。
    #[test]
    fn the_unavailable_event_is_throttled_per_model() {
        let (store, dir) = store_at("throttle");
        let a = key("a", 0, &["opus"]);
        store.upsert_key(a.clone()).unwrap();
        store.upsert_key(breaker(key("b", 1, &["throttle-probe"]), 90_000)).unwrap();

        for i in 1..=5 {
            assert!(
                reject_if_unserviceable(
                    &store,
                    CategoryType::ClaudeCli,
                    "throttle-probe",
                    std::slice::from_ref(&a)
                )
                .is_some(),
                "第 {i} 次也必须回 503 —— 节流只管事件，不管响应"
            );
        }
        let n = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .filter(|e| e.detail.contains("throttle-probe"))
            .count();
        assert_eq!(n, 1, "5 次请求只该落 1 条事件，实得 {n} 条");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 复现用户 2026-09-01 实报那条链：**降级到别的模型时必须留一条 warning**。
    ///
    /// 现场（日志里 31 轮完全相同）：用户选 `grok-4.6`，唯一支持它的 `luckyg` 被自己的网关
    /// 连续 400 → 转移到 `agentrouter`（只有 glm-5.3 等）→ 兜底改写 → **流式返回 200**。
    /// 那行 route 是绿色的「成功」，「兜底改写为 glm-5.3」只是模型段里一小句 ——
    /// 用户以为在用 grok，拿到的是 glm。
    ///
    /// `reject_if_unserviceable` 结构上拦不住它：它只在**进入候选循环之前**判一次，
    /// 那一刻 luckyg 还没失败、`may_serve` 为真、放行是对的。降级发生在**第二跳**。
    #[test]
    fn a_downgrade_to_another_model_leaves_a_warning() {
        let (store, dir) = store_at("downgrade");
        // 复刻用户的真实两条 Key（含 priority：luckyg 反而更靠后，靠模型感知路由排到前面）。
        let mut owner = key("luckyg", 1000, &["grok-4.5", "grok-4.6"]);
        owner.name = "luckyg".into();
        let mut other = key("agentrouter", 999, &["glm-5.3", "gpt-5.6-sol"]);
        other.name = "agentrouter(linux.do)".into();
        other.default_model = Some("glm-5.3".into());
        store.upsert_key(owner).unwrap();
        store.upsert_key(other.clone()).unwrap();

        // 第二跳：请求 grok-4.6 落到 agentrouter 上，被改写成 glm-5.3。
        note_silent_downgrade(&store, CategoryType::ClaudeCli, "grok-4.6", &other, "glm-5.3");

        let ev = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .find(|e| e.kind == "warning")
            .expect("降级必须留痕 —— 那行 route 是绿色成功，没人会去逐行读");
        assert!(ev.detail.contains("grok-4.6"), "要说清用户要的是什么：{}", ev.detail);
        assert!(ev.detail.contains("glm-5.3"), "也要说清实际用了什么：{}", ev.detail);
        assert!(
            ev.detail.contains("luckyg"),
            "要指出谁本来能服务它 —— 那是「该去修哪条 Key」的唯一线索：{}",
            ev.detail
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 反向三条：**不该留痕的情形一条都不许留**（否则日志被刷成噪音）。
    #[test]
    fn a_downgrade_note_is_not_left_for_names_we_never_advertised() {
        let (store, dir) = store_at("no_downgrade");
        let mut other = key("k", 0, &["glm-5.3"]);
        other.default_model = Some("glm-5.3".into());
        store.upsert_key(other.clone()).unwrap();

        // ① 客户端自己编的名字（CC 的带日期后缀家族名）—— 被三档/default_model 改写是
        // 那两个机制存在的**全部理由**，对它留痕等于每次会话刷屏。
        note_silent_downgrade(
            &store,
            CategoryType::ClaudeCli,
            "claude-sonnet-4-5-20250929",
            &other,
            "glm-5.3",
        );
        // ② 没被换掉（要的就是它）。
        note_silent_downgrade(&store, CategoryType::ClaudeCli, "glm-5.3", &other, "glm-5.3");
        // ③ 空模型名（下游压根没给）。
        note_silent_downgrade(&store, CategoryType::ClaudeCli, "", &other, "glm-5.3");

        assert_eq!(
            store
                .list_events(CategoryType::ClaudeCli)
                .into_iter()
                .filter(|e| e.kind == "warning")
                .count(),
            0,
            "这三种都不是「用户选的模型被换掉」，一条都不该留"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **应用内选定的名字也算「我们宣称过」，哪怕它此刻不在并集清单里。**
    ///
    /// 这是 2026-09-01 用户实报那条链**真正**的形态：那一刻 luckyg 的模型列表还没拉到
    /// `grok-4.5`，于是它在 `active_models` 里、却不在 `discoverable_models` 里 ——
    /// 只查并集的话这道门会把唯一一条线索也挡掉，用户每次都拿到 glm 却什么提示都没有。
    ///
    /// v0.1.48 曾改用「自动清掉那个选择」来处理同一场景，v0.1.50 已撤销（见
    /// [`note_silent_downgrade`] 的文档）—— 留痕足够，不该替用户改设置。
    #[test]
    fn an_in_app_selection_counts_as_advertised_even_before_the_model_list_catches_up() {
        let (store, dir) = store_at("active_not_yet_listed");
        // ⚠️ 名字必须与别的用例互不相同：节流表是进程级 static（见下一条用例的注释）。
        let mut k = key("k", 0, &["glm-5.3"]);
        k.default_model = Some("glm-5.3".into());
        store.upsert_key(k.clone()).unwrap();
        store.set_active_model(CategoryType::ClaudeCli, "stale-pick-probe").unwrap();
        // 前提①：这个名字**不在**并集清单里 —— 否则本用例走的是旧判据，压不到新放宽的那一支。
        assert!(
            !discoverable_models(&[k.clone()]).iter().any(|m| m == "stale-pick-probe"),
            "夹具前提破了：这个名字不该出现在并集清单里"
        );

        note_silent_downgrade(&store, CategoryType::ClaudeCli, "stale-pick-probe", &k, "glm-5.3");

        let ev = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .find(|e| e.kind == "warning")
            .expect("应用内选的名字被换掉时必须留痕 —— 那是用户在我们界面上选的，不是客户端编的");
        assert!(ev.detail.contains("stale-pick-probe"), "要说清用户要的是什么：{}", ev.detail);
        assert!(ev.detail.contains("glm-5.3"), "也要说清实际用了什么：{}", ev.detail);
        // 🔴 「没有任何 Key 能服务它」这一支的处置与「支持者挂了」**完全不同**，文案必须分支。
        // 合成一句会拼出「…没有任何已启用 Key 能服务它，**那条 Key** 此刻不可用」——「那条」无所指。
        assert!(
            ev.detail.contains("列表还没拉到") || ev.detail.contains("已被停用"),
            "无支持者时要给出这一支的成因与恢复办法：{}",
            ev.detail
        );
        assert!(
            !ev.detail.contains("那条 Key 此刻不可用"),
            "无支持者时不许说「那条 Key」—— 没有那条 Key：{}",
            ev.detail
        );
        // 🔴 **留痕不许顺手改用户的设置**（v0.1.48 那个自清已撤销）。
        assert_eq!(
            store.active_model_of(CategoryType::ClaudeCli).as_deref(),
            Some("stale-pick-probe"),
            "留痕是只读的：模型列表还没拉到时清掉用户刚选的东西，他会以为界面坏了"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 折叠键必须带 `key.id`，否则两条不同 Key 的降级合并后「徽标写 A、正文说 B」。
    ///
    /// **判据只能是源码级的，而这一点本身要说实话**：这条错配今天**行为上不可达** ——
    /// 折叠只合并**紧邻**的上一条，而 ① `should_announce` 的节流键是「分类 + 模型」
    /// （不含 Key），60s 内第二条 Key 的降级压根不会落；② 60s 之后中间必然已夹了
    /// 本次请求那条 route 成功事件。也就是说这个修复**摘掉的是一个隐患，不是一个活缺陷**，
    /// 写不出能变红的行为用例（能写出来就说明前两个前提已经被改坏了）。
    ///
    /// 把「不可达」如实写在这里，是为了让下一个人知道：改动节流键或事件顺序时，
    /// 这条判据是他唯一的护栏。
    #[test]
    fn two_keys_downgrading_the_same_model_must_not_merge_into_one_row() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!("model_pool.rs"));
        let call = src
            .split("fn note_silent_downgrade")
            .nth(1)
            .and_then(|s| s.split("\npub").next())
            .expect("函数还在吗");
        let collapse = call
            .split("Some(format!(\"downgrade:")
            .nth(1)
            .expect("折叠键还是这个形状吗");
        assert!(
            collapse.starts_with("{}:{}:{bare}\", category.as_str(), key.id)"),
            "折叠键要带 key.id —— 合并会刷新 detail 却保留第一条的 key_id：{collapse:.60}"
        );
    }

    /// 🔴 **无支持者那支不许承诺「自动拉取」** —— 全仓没有这条路径。
    ///
    /// v0.1.49 那句原文是「常见成因是那条 Key 的模型列表还没拉到（新加的 Key 会自动拉取）」。
    /// 取证：落盘那条 `fetch_models` 命令**前端零调用**（`bridge.ts` 里 `fetchModels`
    /// 定义了但没有任何 caller），唯一入口是 Key 编辑器里用户手点的「拉取」按钮，
    /// 走 `fetch_models_draft`（不落盘）+ 保存；`save_key` / `toggle_key` / `check_one`
    /// 都不写 `models`。也就是说「等它自动拉」是**把用户送去等一件永不发生的事**，
    /// 而这条事件是他手里唯一的线索 —— 同本仓「指错方向的提示比没有提示更糟」那条。
    #[test]
    fn the_no_owner_hint_must_not_promise_auto_fetch() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!("model_pool.rs"));
        let hint = src
            .split("let why = if owners.is_empty()")
            .nth(1)
            .and_then(|s| s.split("} else {").next())
            .expect("无支持者那支的文案还在这个形状里吗");
        assert!(
            !hint.contains("自动拉取"),
            "别再承诺自动拉取（全仓无此路径）：{hint}"
        );
        assert!(
            hint.contains("不会自动更新") && hint.contains("拉取"),
            "要如实说明并给出可执行动作（手点「拉取」/ 手填模型名）：{hint}"
        );
    }

    /// 🔴 `fetch_models` 写模型清单却**不过** [`crate::client_resync::sync_after`] ——
    /// 今天无害只因为前端从不调它。谁接上按钮，这条判据就变红。
    ///
    /// 它是 `Store::set_models` 的唯一调用链（`service::fetch_models_for_key`），
    /// 也就是**唯一会改并集清单却不重写客户端配置**的写入点。接上之后的失效形态很具体：
    /// 用户点「拉取」拿到 `grok-4.6` 并保存 → Codex 菜单里仍然没有它（目录是
    /// `applied on startup only`，得有人重写那个文件）—— 正是 `client_resync` 存在的理由。
    ///
    /// **刻意不现在就接上**：`lib.rs` 棘轮余量为 0，而今天这条命令不可达、零用户可感收益。
    /// 判据钉住「不可达」这个前提本身，比钉住一个用不到的调用更有用（同
    /// `the_two_sse_invariants_must_stay_derived_not_assumed` 的思路：钉来源，不钉结论）。
    #[test]
    fn fetch_models_stays_unreachable_until_someone_wires_the_resync() {
        let bridge = include_str!("../../src/lib/bridge.ts");
        // 定义行本身不算调用（`fetchModels: (keyId) => call<...>("fetch_models", ...)`）。
        let callers = bridge.matches("api.fetchModels(").count()
            + include_str!("../../src/components/KeyEditor.tsx")
                .matches("api.fetchModels(")
                .count();
        assert_eq!(
            callers, 0,
            "有人接上了落盘版模型拉取 —— 请让它一并过 client_resync::sync_after，\
             否则拉到的新模型不会出现在 Codex 菜单/桌面端选择器里"
        );
    }

    /// 🔴 用户**显式配了映射**时不许报警 —— 那是他自己的意图，不是我们悄悄换的。
    ///
    /// ⚠️ 这条是注入实测补上来的：上面那三条反向用例**全被前面的门先挡住**
    /// （`bare == real` / 判据②「没宣称过」），于是把 `Fallback` 那道门改成恒不早退，
    /// 36 条**照样全绿** —— 判据重叠、压根没压到那一支。同 CLAUDE.md 里
    /// 「注入不变红时先怀疑判据本身重复或没压到边界」那条。
    ///
    /// 要压到它必须同时满足：名字在宣称清单里（判据②过）、`bare != real`（第一道门过）、
    /// 而 `confidence` 不是 `Fallback` —— 映射命中（`Mapping` → `Native`）正是这个组合。
    #[test]
    fn an_explicit_mapping_is_the_users_own_intent_not_a_silent_downgrade() {
        let (store, dir) = store_at("mapped");
        // ⚠️ 模型名必须与别的用例**互不相同**：节流表是进程级 static，共用一个名字会让
        // 本条被 60s 窗口跳过 —— 那时「0 条事件」是假绿，注入也照样绿。
        // 同 CLAUDE.md 里 `DENIED_TOTAL` 那条 flaky（进程级计数器的断言必须隔离）。
        let mut k = key("k", 0, &["glm-5.3"]);
        // 用户手配：我要 mapped-probe，就发 glm-5.3 给上游。
        k.mappings = vec![mapping("mapped-probe", "glm-5.3")];
        store.upsert_key(k.clone()).unwrap();
        // 前提：这个名字确实被宣称了（否则判据②会先挡住，这条用例又变成空转）。
        assert!(
            discoverable_models(&[k.clone()]).iter().any(|m| m == "mapped-probe"),
            "映射的对外名必须在宣称清单里，否则本用例压不到 Fallback 那道门"
        );

        note_silent_downgrade(&store, CategoryType::ClaudeCli, "mapped-probe", &k, "glm-5.3");

        assert_eq!(
            store
                .list_events(CategoryType::ClaudeCli)
                .into_iter()
                .filter(|e| e.kind == "warning")
                .count(),
            0,
            "映射命中是用户的明确配置，报警等于对他自己配的东西发警告"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 接线判据：成功路径必须调 [`note_silent_downgrade`]。
    ///
    /// 上面那几条都直接调函数 —— 把 `proxy.rs` 里那一行删掉它们照样全绿，而那正是缺陷本体
    /// （降级照旧发生、只是没人说）。本仓第 17 次盯同一类盲区。
    #[test]
    fn the_success_path_must_note_silent_downgrades() {
        let src = prod(include_str!("proxy.rs"));
        assert_eq!(
            src.matches("model_pool::note_silent_downgrade(").count(),
            1,
            "成功路径必须留痕降级，且只该有一处调用点"
        );
        // 位置必须在 `log_success` 闭包里：挂到别处（比如候选循环）会对流式与非流式
        // 两条成功路径漏掉一条 —— 那种漏是静默的。
        let at = src.find("let log_success =").expect("log_success 改名了，请同步本判据");
        let end = src[at..].find("\n    };").map(|i| at + i).unwrap_or(src.len());
        assert!(
            src[at..end].contains("note_silent_downgrade("),
            "必须挂在 log_success 里 —— 那是流式与非流式共用的唯一成功出口"
        );
    }

    /// Codex 目录的档位与窗口两维必须**按模型**收窄（`owners_of`），不能拿全池算。
    ///
    /// 拿全池算的失效都是静默的：一条 Chat 协议的备用 Key 抹掉全部模型的档位声明；
    /// 一条低窗口的 Key 把独有模型的窗口压小 → Codex 提前压缩上下文。
    #[test]
    fn the_codex_catalog_must_scope_both_dimensions_per_model() {
        let src = prod(include_str!("tools/codex_catalog.rs"));
        assert_eq!(
            src.matches("owners_of(name, keys)").count(),
            2,
            "档位与窗口各一处；少一处就是那一维仍在拿全池算"
        );
    }
}

