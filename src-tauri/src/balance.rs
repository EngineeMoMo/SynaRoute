//! 上游余额查询（对齐 cc-switch 的 `usage_script`，但不执行用户代码）。
//!
//! ## 为什么是声明式而不是跑用户的 JS
//!
//! cc-switch 让用户在界面里写一段 JavaScript（`{request, extractor}`）并内置引擎执行。
//! 取证它本机数据库里的内置「通用模板」后可以看到，extractor 的实质就是几个 `??` 回退：
//!
//! ```javascript
//! const remaining = response?.remaining ?? response?.quota?.remaining ?? response?.balance;
//! const unit = response?.unit ?? response?.quota?.unit ?? "USD";
//! ```
//!
//! 这类逻辑用「候选字段链」完全覆盖，而内置 JS 引擎会给一个本地密钥托管应用
//! 增加一个任意代码执行入口（cc-switch 的脚本里还直接嵌了明文 apiKey）。
//! 故这里抄它的**数据契约**（占位符、多重回退、`{isValid, remaining, unit}`），
//! 不抄它的执行模型。
//!
//! ## 失败一律可见
//!
//! 取不到值时返回 `BalanceResult::failed(原因)`，`remaining` 保持 `None`。
//! **绝不返回 0** —— 显示「余额 0」会让用户以为额度真的用光了，
//! 比不显示更糟。

use crate::model::{BalanceQuery, BalanceResult, ProviderKey, DEFAULT_BALANCE_TIMEOUT_SECS};
use serde_json::Value;
use std::time::Duration;

/// 剩余额度的候选字段链（按优先级）。
///
/// 前几条对齐 cc-switch 通用模板 extractor 的 `??` 顺序；其后覆盖常见中转站面板
/// （newapi / oneapi 系放 `data.quota`，部分站点用 `credit`）与几家第三方厂商的实测结构。
///
/// **顺序有讲究**：`balance_infos.0.total_balance` 必须排在泛化的 `balance` 之前 ——
/// DeepSeek 的响应里 `balance_infos` 是数组、根上没有 `balance`，但若将来某家同时
/// 有两者，更具体的那个才是对的。
const REMAINING_CANDIDATES: &[&str] = &[
    "remaining",
    "quota.remaining",
    // DeepSeek：`{"balance_infos":[{"currency":"CNY","total_balance":"4.28",...}]}`（实测）
    "balance_infos.0.total_balance",
    // SiliconFlow 系：`{"data":{"balance":"0","chargeBalance":"5","totalBalance":"5"}}` ——
    // `totalBalance` = 赠送额度 `balance` + 充值额度 `chargeBalance`，才是真实剩余。
    // **必须排在 `data.balance` 之前**：赠送额度用尽时 `data.balance="0.00"` 会被 as_number
    // 解析成 0 并命中返回（候选链取首个「可转数字」，0 也算命中、不会跳过），把「还有充值余额」
    // 误显成 0，违反本模块「绝不返回 0」。cc-switch 的 siliconflow 函数也只读 totalBalance。
    "data.totalBalance",
    "totalBalance",
    // 月之暗面 Kimi：`GET /v1/users/me/balance` 返回
    // `{"code":0,"data":{"available_balance":49.58,"voucher_balance":46.58,"cash_balance":3.0},…}`
    // —— **snake_case**，与下面 Novita 的 camelCase `availableBalance` 是两个不同的字段。
    //
    // 这条是**我们自己挖的坑**：`VENDOR_ENDPOINTS` 里给 `api.moonshot.cn/.ai` 加了这个端点
    // （cc-switch 没有，是本项目补的），却没在候选链里加对应字段。结果是 Kimi 用户零配置
    // 拿到了**正确的 URL、200 的响应**，然后卡在「上游返回里找不到余额字段」——
    // 比根本不支持更难排查，因为报错指向「去改取值路径」，而地址其实是对的。
    //
    // 🔴 **绝不可加进 `SCALED_FIELDS`**：Novita 的 camelCase 版是 0.0001 USD 整数、要除
    // 10000；Kimi 这个是**元的浮点数**，缩放会把 49.58 显示成 0.0049（看着像额度耗尽）。
    // 两个字段名只差一个下划线、含义与量纲都不同，按名字模式推断必错，故分开列并各自注明。
    //
    // 排在泛化 `balance`/`data.balance` **之前**：同时返回两者的站点里，`available_balance`
    // （可用 = 代金券 + 现金）才是真实可花的额度。同文件开头「更具体的优先」的一贯口径。
    "available_balance",
    "data.available_balance",
    "balance",
    "data.balance",
    "data.remaining",
    "data.quota.remaining",
    // ⚠️ `data.quota` / `quota`：NewAPI/OneAPI 架构里这是**内部计费单位**，不是 USD。
    // 换算比率由站点的公开接口 `/api/status` 给出（`quota_per_unit`，实测两站都是 500000），
    // 由 `newapi_quota_needs_scaling` + `read_newapi_quota_unit` 现场读取后缩放 ——
    // **不硬编码 500000**（站长可改），也不再「刻意不缩放」（那会显示成 308240000 这种
    // 荒谬大数，与 hard_limit_usd 那条是同一类错误）。读不到比率时报失败，不猜。
    "data.quota",
    "credit",
    "data.credit",
    // OpenRouter：`{"data":{"limit_remaining":...}}`
    "data.limit_remaining",
    "total_available",
    "data.total_available",
    // ⚠️ **`hard_limit_usd` 刻意不在这条链里** —— 它是「配额上限」，从来不是余额。
    // 两次真机实测都证明拿它（或 `上限 − 已用`）当余额会给出离谱的错数字：
    //   - sotamodel.net：`hard_limit_usd = 10000`（NewAPI 默认值）→ 显示「10000 USD」；
    //   - agentrouter.org：`hard_limit_usd = 100000000`（1e8「无限额」哨兵）
    //     → `1e8 − 1179.52 = 99,998,820.48`，而网页上的真实余额是 **616.48**。
    //     （那个 1179.52 恰好是「历史消耗」，说明减法本身算对了，是**被减数**没有意义。）
    // 处置见 `billing_limit_is_not_a_balance`：宁可如实报「这站点没给余额接口」并指路，
    // 也不给一个用户会当真的错数字。
    // ---- 以下三条对齐 cc-switch 的硬编码厂商实现（2026-08-17 从其源码
    // `src-tauri/src/services/balance.rs` 逐个函数核对，非文档推测）----
    //
    // cc-switch 为这 5 家各写一个函数、按 base_url 子串路由：DeepSeek（我们已有
    // `balance_infos.0.total_balance`）、SiliconFlow（已有 `data.totalBalance`）、
    // OpenRouter（已有 `data.limit_remaining`，但它实际算的是 total_credits −
    // total_usage，见下）、StepFun（`balance`，已被泛化项覆盖）、Novita（缺）。
    //
    // Novita AI：`{"availableBalance": 123456}`，单位是 **0.0001 USD**
    // （cc-switch 除以 10000）。我们的候选链只取原始数字，故这里取到的是
    // 「万分之一美元」的整数 —— 单位换算见 `as_number` 下方的 SCALED_FIELDS。
    //
    // ⚠️ 与上面 Kimi 的 snake_case `available_balance` **不是同一个字段**：那个是元的浮点、
    // 不缩放。改动这里时不要顺手把两者合并或统一处理。
    "availableBalance",
    "data.availableBalance",
    // StepFun：`{"balance": ...}` 已由上面的泛化 `balance` 命中，无需单列。
    //
    // OpenRouter 的 `/api/v1/credits`：`{"data":{"total_credits":X,"total_usage":Y}}`，
    // 剩余 = X − Y。**这是个减法、不是单字段**，候选链表达不了 ——
    // 由 `derive_openrouter_remaining` 在候选链之前单独处理（见 extract_balance）。
];

/// 需要按固定倍数缩放的字段（字段名 → 除数）。
///
/// 为什么需要它：多数站点的余额字段就是「多少钱」，但少数厂商用整数最小单位存以避免浮点。
/// Novita AI 的 `availableBalance` 是 0.0001 USD 的整数倍（cc-switch 的
/// `novita` 函数里明确 `/ 10000.0`）——不缩放会把 12.3456 USD 显示成 123456，
/// 用户看到一个荒谬的大数只会以为程序坏了。
///
/// 只对**精确匹配的字段名**生效，不做前缀/子串匹配：泛化匹配会把别家同名字段
/// 也误缩放，那比不缩放更糟（12.34 显示成 0.0012）。
const SCALED_FIELDS: &[(&str, f64)] = &[
    ("availableBalance", 10_000.0),
    ("data.availableBalance", 10_000.0),
];

/// 货币单位的候选字段链。取不到时默认 USD（与 cc-switch 通用模板一致）。
const UNIT_CANDIDATES: &[&str] = &[
    "unit",
    "quota.unit",
    "data.unit",
    "currency",
    "data.currency",
    // DeepSeek 把币种放在同一个数组元素里
    "balance_infos.0.currency",
];

/// 按 `base_url` 自动识别厂商的余额端点（域名子串 → 完整 URL 模板）。
///
/// ## 为什么需要它
///
/// 这是 cc-switch 唯一真正比我们好用的地方（2026-08-17 读其源码
/// `src-tauri/src/services/balance.rs` 核对）：它的 `detect_provider` 按 `base_url`
/// 子串匹配，用户**零配置**就能查余额；而我们要求用户自己判断「我这个站属于哪一类」
/// 并手选模板 —— 而这恰恰是用户最不可能知道的信息（同一个中转站可能是 NewAPI 架构、
/// 也可能自研面板，端点路径没有任何规律可循）。真机反馈的「余额查询一直不行」
/// 大概率就是选错了模板。
///
/// ## 判据与取舍
///
/// - **只在用户没手选过模板时生效**（`template` 为空或 `"auto"`）。用户明确选过的
///   一律尊重 —— 自动识别猜错时用户还有手动出路，反过来会让「我明明选了 NewAPI」失效。
/// - 用 `{{origin}}` 而非 `{{baseUrl}}`：这些余额端点都在**域名根**下，而转发用的
///   baseUrl 常带 `/v1`、`/anthropic` 之类后缀（DeepSeek 就是），用 baseUrl 必然 404。
/// - 匹配**域名子串**而非全等：同一家常有多个域名（`api.foo.com` / `foo.com` /
///   区域镜像），全等匹配会把绝大多数真实配置判成未知。
/// - 顺序有讲究：更具体的域名必须排在更宽泛的之前（如 `openrouter.ai` 若将来出现
///   `openrouter.ai.cn` 这类，具体的要先命中）。
///
/// 前 5 家逐条对齐 cc-switch 的硬编码函数；其后是本项目实测补的。
const VENDOR_ENDPOINTS: &[(&str, &str, &str)] = &[
    // (域名子串, URL 模板, 认证方式)
    //
    // ---- 以下 5 家对齐 cc-switch（其 detect_provider + 各 provider 函数）----
    ("api.deepseek.com", "{{origin}}/user/balance", "bearer"),
    // cc-switch 的 StepFun 分支匹配 .ai/.com 两个域名但请求恒发 .com
    ("api.stepfun.ai", "https://api.stepfun.com/v1/accounts", "bearer"),
    ("api.stepfun.com", "{{origin}}/v1/accounts", "bearer"),
    ("api.siliconflow.cn", "{{origin}}/v1/user/info", "bearer"),
    ("api.siliconflow.com", "{{origin}}/v1/user/info", "bearer"),
    ("openrouter.ai", "{{origin}}/api/v1/credits", "bearer"),
    ("api.novita.ai", "{{origin}}/v3/user/balance", "bearer"),
    // ---- 以下是本项目实测补的（cc-switch 没有）----
    // 智谱 GLM：开放平台的额度接口
    ("open.bigmodel.cn", "{{origin}}/api/paas/v4/account/balance", "bearer"),
    // 月之暗面 Kimi
    ("api.moonshot.cn", "{{origin}}/v1/users/me/balance", "bearer"),
    ("api.moonshot.ai", "{{origin}}/v1/users/me/balance", "bearer"),
    // 官方 Anthropic：走组织信息端点，认证头也不同（x-api-key）
    ("api.anthropic.com", "{{origin}}/v1/organizations/me", "x-api-key"),
];

/// 认不出具体厂商时的**兜底端点链**（按序探测，命中即停）。
///
/// ## 为什么是一条链，而不是一个端点（本轮改动，有取证）
///
/// 原先只有一条 `/v1/dashboard/billing/subscription`，而它对中转站是**错的那一条**：
/// NewAPI 系在这个端点返回的是 `hard_limit_usd`（配额**上限**），不是余额。
/// 于是真机上大量 su2api / NewAPI 账号一律显示「10000 USD」，且那个数字永不变化。
///
/// **判据来自用户本机 cc-switch 库**（2026-08-22 直接读 `~/.cc-switch/cc-switch.db`
/// 的 `providers.meta`，非文档推测）：它给每个供应商存一段可编辑的 `usage_script`，
/// 而用户那两个站（`Sub2API` = sub.100xlabs.space、`「林夕」公益站` = k40.shengqainbang.cn）
/// 存的都是：
///
/// ```jsonc
/// {"usage_script":{"enabled":true,"language":"javascript",
///   "code":"({ request:{ url:\"{{baseUrl}}/v1/usage\", method:\"GET\", … }, extractor: … })",
///   "timeout":10, "autoQueryInterval":30 }}
/// ```
///
/// —— 端点是 **`/v1/usage`**。此前我们抄了它 extractor 的 `??` 字段链（见本文件开头注释），
/// **却没抄端点**，于是「取值路径」怎么调都没用：地址本身就打错了。
/// 而错误文案还会把人指向「去改取值路径」，方向完全相反。
///
/// ## 探测的代价与遏制
///
/// 原注释写着「刻意不做多端点轮询：那会对上游发 N 次请求」。这个顾虑仍然成立，
/// 但**兜底端点本身是错的**这件事比它严重得多 —— 省下的请求换来的是一个恒定错误的数字。
/// 故改为探测 + 三重遏制：
/// 1. **命中即停**，且把命中的模板写回 `BalanceQuery.url`（见 `resolved_url_template`），
///    此后每次只发 1 个请求 —— 探测是一次性成本，不是每轮成本；
/// 2. 只在「用户没填 url」**且**「域名认不出」时才探测。用户填过的、能按域名认出的
///    一律单发一次（`VENDOR_ENDPOINTS` 那 11 条精确命中的不受影响）；
/// 3. 链长封顶 3 条，且余额端点不是推理端点、不按 token 计费。
///
/// 顺序即「命中概率 × 结果正确性」：`/v1/usage` 既最常见又直接给余额；
/// `hard_limit_usd` 那条排最后 —— 它能出数但那是上限，属最后的兜底（且已有
/// `adjust_billing_limit_with_usage` 再去扣一次已用）。
const FALLBACK_ENDPOINTS: &[(&str, &str)] = &[
    // cc-switch 给中转站实配的那条（上面的取证）。多数 su2api / 公益站走这条。
    ("{{origin}}/v1/usage", "bearer"),
    // NewAPI / OneAPI 面板自身的用户信息接口，部分站点用 API Key 也能读。
    ("{{origin}}/api/user/self", "bearer"),
    // OpenAI 兼容计费层。**返回的是上限而非余额**，故排最后（见上方顺序说明）。
    ("{{origin}}/v1/dashboard/billing/subscription", "bearer"),
];

/// 按 `base_url` 猜该站点的余额端点。返回 `(url 模板, 认证方式)`。
///
/// 命中 [`VENDOR_ENDPOINTS`] 里的域名就用那家的；认不出则返回
/// [`FALLBACK_ENDPOINTS`] 的**第一条**。
///
/// ⚠️ 认不出时「只返回第一条」是有损的 —— 真正的处置是按序探测整条链
/// （见 [`resolve_endpoint_candidates`]）。本函数保留是因为它已被 UI 预览与
/// 迁移逻辑当作「这个域名会怎么查」的展示入口，那里只需要一个代表值。
/// **查询路径不要用它**，用 `resolve_endpoint_candidates`。
///
/// 大小写不敏感：用户可能把域名写成 `API.DeepSeek.com`。
pub fn detect_balance_endpoint(base_url: &str) -> (&'static str, &'static str) {
    let lower = base_url.to_ascii_lowercase();
    for (domain, url, auth) in VENDOR_ENDPOINTS {
        if lower.contains(domain) {
            return (url, auth);
        }
    }
    FALLBACK_ENDPOINTS[0]
}

/// 域名能否被精确识别（命中 [`VENDOR_ENDPOINTS`]）。
fn vendor_is_known(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    VENDOR_ENDPOINTS.iter().any(|(domain, _, _)| lower.contains(domain))
}

/// 本次查询要按序尝试的 `(url 模板, 认证方式)` 候选列表。
///
/// 三条规则，优先级从上到下：
///
/// 1. **用户填了 url → 只用他填的那一条，绝不探测。** 哪怕它是错的：那样他才能从报错里
///    看出自己填错了。替他悄悄换成能用的地址是「我改的不生效」这类静默失效，
///    而这条规则本身也是用户唯一的逃生口（自动识别猜错时他还有手动出路）。
/// 2. **域名能精确识别 → 只用那一条。** `VENDOR_ENDPOINTS` 里 11 条是逐个取证过的，
///    更具体的判据必须胜过泛化探测；对这些站点探测纯属多打请求。
/// 3. **都不满足 → 返回整条 [`FALLBACK_ENDPOINTS`] 按序试。** 这是中转站的主场景，
///    也是原先「只试一条且那条是错的」造成「恒显示 10000 USD」的地方。
///
/// `auth` 独立判空：用户只填了 auth 没填 url 时，把他的 auth 覆盖到每个候选上
/// —— 他填 auth 的意图是「这个站的认证方式是这样」，与试哪个路径无关。
///
/// 抽成纯函数是为了让三条分支能脱离网络单测（`query_balance` 要真打上游）。
fn resolve_endpoint_candidates<'a>(cfg: &'a BalanceQuery, base: &str) -> Vec<(&'a str, &'a str)> {
    let user_url = cfg.url.trim();
    let user_auth = cfg.auth.trim();
    // auth 为空时由候选自带；非空则覆盖全部候选。
    let with_auth = |url: &'a str, auth: &'a str| -> (&'a str, &'a str) {
        (url, if user_auth.is_empty() { auth } else { user_auth })
    };

    if !user_url.is_empty() {
        // 规则 1：用户填了地址 → 就这一条
        let detected_auth = detect_balance_endpoint(base).1;
        return vec![with_auth(user_url, detected_auth)];
    }
    if vendor_is_known(base) {
        // 规则 2：域名认得出 → 就这一条
        let (url, auth) = detect_balance_endpoint(base);
        return vec![with_auth(url, auth)];
    }
    // 规则 3：认不出 → 整条链按序试
    FALLBACK_ENDPOINTS.iter().map(|(url, auth)| with_auth(url, auth)).collect()
}

/// Key 是否有效的候选字段链。
///
/// `is_available` 是 DeepSeek 的字段名（实测），与 cc-switch 文档里的
/// `is_active` / `isValid` 并列。
const VALID_CANDIDATES: &[&str] = &[
    "is_active",
    "isValid",
    "is_available",
    "data.is_active",
    "active",
    "enabled",
];

/// 总额度与已用额度的候选链（对齐 cc-switch 返回契约里的 `total` / `used`）。
const TOTAL_CANDIDATES: &[&str] = &["total", "data.total", "quota", "data.quota_total"];
const USED_CANDIDATES: &[&str] = &["used", "data.used", "used_quota", "data.used_quota"];

/// 套餐名的候选链（cc-switch 的 `planName`，多套餐站点会给）。
const PLAN_CANDIDATES: &[&str] = &["planName", "plan_name", "data.group", "group", "data.plan"];

/// 无效原因的候选链（cc-switch 的 `invalidMessage`）。
const INVALID_MSG_CANDIDATES: &[&str] = &["invalidMessage", "message", "data.message", "error"];

/// 按点分路径取值，**支持数组下标**（如 `"balance_infos.0.total_balance"`）。
///
/// 单独抽出来而不是内联进候选链循环：用户自定义的 `remaining_path` 走的是同一套
/// 解析，两处各写一遍就是漂移的温床。
///
/// 数组下标是实测需求：DeepSeek 的余额在 `balance_infos[0].total_balance`，
/// 纯对象取值拿不到。cc-switch 靠用户写 JS 表达式解决，我们是声明式的，
/// 故必须在路径语法里支持 —— 否则这类结构只能让用户手填、而他也填不出来。
fn pick<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.split('.') {
        cur = match cur {
            // 纯数字段视为数组下标；对象上的数字键（少见但合法）由 get(seg) 兜底
            Value::Array(items) => match seg.parse::<usize>() {
                Ok(i) => items.get(i)?,
                Err(_) => return None,
            },
            _ => cur.get(seg)?,
        };
    }
    Some(cur)
}

/// 把 JSON 值宽松地转成 f64。
///
/// 必须容忍字符串形态：不少中转站把余额写成 `"12.34"`（避免大数精度问题），
/// 只认 `as_f64()` 会在这些站点上静默取不到值。
fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// 按候选链找第一个能转成数字的字段。
fn find_number(root: &Value, candidates: &[&str]) -> Option<f64> {
    candidates.iter().find_map(|p| pick(root, p).and_then(as_number))
}

/// 同 [`find_number`]，但对 [`SCALED_FIELDS`] 里的字段按其除数缩放。
///
/// 分成两个函数而不是给 `find_number` 加参数：`find_number` 还被单位/总额/已用等
/// 候选链复用，那些字段没有缩放语义，混在一起会让「哪些会被缩放」变得不可预测。
fn find_number_scaled(root: &Value, candidates: &[&str]) -> Option<f64> {
    candidates.iter().find_map(|p| {
        let raw = pick(root, p).and_then(as_number)?;
        let divisor = SCALED_FIELDS
            .iter()
            .find(|(name, _)| name == p)
            .map(|(_, d)| *d)
            .unwrap_or(1.0);
        Some(raw / divisor)
    })
}

/// OpenRouter 的 `/api/v1/credits` 要做减法：剩余 = `total_credits` − `total_usage`。
///
/// 为什么单独一个函数：候选链是「取某个字段的值」，表达不了两字段相减。cc-switch 为此
/// 专门写了 `openrouter` 函数（`remaining = total_credits - total_usage`，
/// 且 `is_valid = remaining > 0`），我们对齐它的语义。
///
/// 判据要求**两个字段都在**才算命中：只有其中一个时无法得出剩余，返回 `None` 让候选链
/// 继续尝试（例如某天 OpenRouter 直接给了 `remaining`）。兼容 `data` 包裹与根上两种形态。
fn derive_openrouter_remaining(body: &Value) -> Option<f64> {
    // `data` 包裹优先（OpenRouter 实际返回带 data），根上作为兜底
    for root in [body.get("data").unwrap_or(body), body] {
        let credits = pick(root, "total_credits").and_then(as_number);
        let usage = pick(root, "total_usage").and_then(as_number);
        if let (Some(c), Some(u)) = (credits, usage) {
            return Some(c - u);
        }
    }
    None
}

/// 按候选链找第一个非空字符串。
fn find_string(root: &Value, candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|p| {
        pick(root, p)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// 按候选链找第一个布尔值（也接受 0/1 与 "true"/"false"）。
fn find_bool(root: &Value, candidates: &[&str]) -> Option<bool> {
    candidates.iter().find_map(|p| {
        pick(root, p).and_then(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => n.as_i64().map(|i| i != 0),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Some(true),
                "false" | "0" | "no" => Some(false),
                _ => None,
            },
            _ => None,
        })
    })
}

/// 从 base_url 里取出「协议 + 域名」，剥掉路径部分。
///
/// **为什么需要它**（实测踩到）：转发端点常带路径前缀，而余额端点在域名根下。
/// DeepSeek 就是这样 —— baseUrl 是 `https://api.deepseek.com/anthropic`，
/// 而余额在 `https://api.deepseek.com/user/balance`。用 `{{baseUrl}}/user/balance`
/// 拼出 `.../anthropic/user/balance` → 404。
///
/// 这是 cc-switch 没有的占位符：它让用户写 JS，能自己 `.replace()` 掉路径；
/// 我们是声明式的，必须把这个能力做进占位符里，否则整类站点都配不出来。
fn origin_of(base_url: &str) -> String {
    let s = base_url.trim();
    // 找到 scheme:// 之后的第一个 '/'，截断于此
    if let Some(rest) = s.split_once("://") {
        let (scheme, host_and_path) = rest;
        let host = host_and_path.split('/').next().unwrap_or(host_and_path);
        return format!("{scheme}://{host}");
    }
    // 没有 scheme 时按「整体就是 host」处理（调用方随后会校验 URL 合法性）
    s.split('/').next().unwrap_or(s).to_string()
}

/// 展开占位符。
///
/// 前两个对齐 cc-switch 文档（§2.5 列了 `{{apiKey}}` / `{{baseUrl}}` /
/// `{{accessToken}}` / `{{userId}}`）；`{{origin}}` 是本项目补的，理由见 [`origin_of`]。
///
/// `base_url` 末尾斜杠先剥掉：模板写的是 `{{baseUrl}}/user/balance`，
/// 若 base_url 本身以 `/` 结尾会拼出 `//user/balance`，部分网关对此返回 404。
pub fn expand_placeholders(
    template: &str,
    base_url: &str,
    api_key: &str,
    access_token: &str,
    user_id: &str,
) -> String {
    template
        .replace("{{baseUrl}}", base_url.trim_end_matches('/'))
        .replace("{{origin}}", &origin_of(base_url))
        .replace("{{apiKey}}", api_key)
        // NewAPI 模板用这两个：它的鉴权不是 API Key 而是面板的 access token + 用户 id
        .replace("{{accessToken}}", access_token)
        .replace("{{userId}}", user_id)
}

/// OpenAI 兼容计费层的**配对端点**：订阅（额度上限）与用量（已消耗）。
///
/// OpenAI 的 billing API 是**两个端点配对**设计的，NewAPI / OneAPI 系照抄了这套契约：
/// - `/v1/dashboard/billing/subscription` → `hard_limit_usd`（额度**上限**，单位 USD）
/// - `/v1/dashboard/billing/usage`        → `total_usage`（已消耗，单位**美分**）
///
/// 真正的剩余 = `hard_limit_usd − total_usage / 100`。
///
/// ## 为什么必须配对查，不能只拿 subscription
///
/// 真机反馈：大量 su2api / NewAPI 系账号查回来都是「余额 10000 USD」。根因是
/// `hard_limit_usd` 是**上限**而非剩余 —— 这类站点给账号配的上限常是一个很大的常数
/// （10000 是最常见的默认值），无论用掉多少它都不变。把它当余额显示，等于告诉用户
/// 「你还有一万美元」，而账号可能早就欠费停用了。这比不显示余额糟得多，
/// 与本模块「绝不返回 0（会让用户以为额度用光）」是同一条原则的另一面：
/// **绝不返回一个虚高的上限当余额**。
///
/// ## 为什么这里可以多发一次请求（与模块开头「刻意不做多端点轮询」不矛盾）
///
/// 那条约束针对的是「**猜**端点」——逐个试候选路径，失败率高、纯浪费。
/// 这里是**同一套 API 的两个必需部分**：拿不到 usage 就算不出剩余，不是可选的优化。
/// 且只在识别出 billing subscription 形态时才发第二次（普通站点一次请求照旧）。
const BILLING_USAGE_PATH: &str = "/v1/dashboard/billing/usage";

/// OpenAI 兼容计费层的已消耗额度（USD）。响应形如 `{"object":"list","total_usage":1234.5}`，
/// **单位是美分**（OpenAI 契约如此，NewAPI 照抄），故除以 100。
fn parse_billing_usage_usd(body: &Value) -> Option<f64> {
    let cents = body
        .get("total_usage")
        .and_then(as_number)
        .or_else(|| body.get("data").and_then(|d| d.get("total_usage")).and_then(as_number))?;
    Some(cents / 100.0)
}

/// 从上游返回的 JSON 里提取余额结果。
///
/// 与网络层分离，故可被单测直接喂各家的真实返回结构（见本文件测试）。
pub fn extract_balance(body: &Value, custom_path: Option<&str>) -> BalanceResult {
    let now = chrono::Utc::now().timestamp_millis();

    // 用户显式指定了路径就只认它：自动探测在这种情况下只会掩盖「路径填错了」。
    let remaining = match custom_path.map(str::trim).filter(|s| !s.is_empty()) {
        Some(path) => match pick(body, path).and_then(as_number) {
            Some(v) => Some(v),
            None => {
                return BalanceResult::failed(format!(
                    "按指定路径 `{path}` 未取到数值（检查路径是否与上游返回结构一致）"
                ))
            }
        },
        // 自动探测：先试 OpenRouter 式的两字段减法（候选链表达不了），再走候选链。
        //
        // 顺序不能反：OpenRouter 的返回里 `total_credits` 会被泛化候选项误当成
        // 「剩余」（它其实是总额），那样已用掉的部分不会被扣掉、余额虚高。
        None => derive_openrouter_remaining(body)
            .or_else(|| find_number_scaled(body, REMAINING_CANDIDATES)),
    };

    let is_valid = find_bool(body, VALID_CANDIDATES);

    let Some(remaining) = remaining else {
        // 上游明确说「这个号无效」时，报的是**那个原因**而不是「找不到字段」——
        // 后者会把用户引向「我路径填错了吗」，而真相是账号本身不能用了。
        if is_valid == Some(false) {
            let why = find_string(body, INVALID_MSG_CANDIDATES)
                .unwrap_or_else(|| "上游报告该账号当前不可用".into());
            let mut r = BalanceResult::failed(why.clone());
            r.is_valid = Some(false);
            r.invalid_message = Some(why);
            return r;
        }
        // 如实说明「连不上」和「取不到字段」的区别，否则用户无从下手改配置。
        return BalanceResult::failed(
            "上游返回里找不到余额字段（可在「取值路径」手填，如 data.balance）",
        );
    };

    BalanceResult {
        ok: true,
        remaining: Some(remaining),
        unit: Some(find_string(body, UNIT_CANDIDATES).unwrap_or_else(|| "USD".into())),
        is_valid,
        // 即使成功也带上：部分站点会「有余额但账号被限」，那时这条要显示出来
        invalid_message: if is_valid == Some(false) {
            find_string(body, INVALID_MSG_CANDIDATES)
        } else {
            None
        },
        plan_name: find_string(body, PLAN_CANDIDATES),
        total: find_number(body, TOTAL_CANDIDATES),
        used: find_number(body, USED_CANDIDATES),
        queried_at: now,
        error: None,
        // 成功结果不存在「瞬时」概念（该标记只用于「没打上游、不代表结论」的失败）。
        transient: false,
        // 由 `query_balance` 在「真探测过且命中」时补上，纯解析层不知道用了哪个端点。
        resolved_url_template: None,
    }
}

/// 本次返回是否是「OpenAI 兼容 billing subscription」形态，即余额值取自
/// `hard_limit_usd`（**额度上限**，不是剩余）。
///
/// 这类返回必须再查一次 `/v1/dashboard/billing/usage` 把已消耗扣掉，否则显示的是上限
/// —— 真机上大量 su2api / NewAPI 账号因此都显示「10000 USD」（见 [`BILLING_USAGE_PATH`]）。
///
/// 判据是「**余额确实取自 hard_limit_usd**」而不是「响应里有 hard_limit_usd」：
/// 若上游同时给了更精确的 `remaining`（候选链里排在前面），那个值本身就是剩余，
/// 再去减一次 usage 会把余额算成负数。
/// `hard_limit_usd` 被视为「不限额哨兵」的下限（USD）。
///
/// ## 为什么需要一条线，而不是「一律信」或「一律不信」
///
/// NewAPI/OneAPI 的 `/v1/dashboard/billing/subscription` 里 `hard_limit_usd` 的语义随
/// token 类型分岔（两站实测 + 站点网页对账得出）：
///
/// - **限额 token**：它就是这条 token 的额度，`上限 − 已用` = **真实剩余**。
///   这条路径**零配置就能算对**，是绝不该砍掉的。
/// - **不限额 token**：站点填一个固定大数当哨兵，与余额毫无关系。实测
///   agentrouter.org = `100000000`（而 `历史消耗 1179.52 + 当前余额 616.48 = 1796`，
///   和 1e8 差 5 个数量级）；sotamodel.net = `10000`（文档记着「花多少都不变」）。
///
/// 响应体里没有字段能直接区分两者，故用数值量级做判据：中转站的 API token 真实额度
/// 几乎不可能 ≥ 1 万美元，而两个已知哨兵恰好都 ≥ 1 万。取值刻意贴着观测值，
/// 而不是拍一个更大的数 —— 定太高（如 1e6）就会让 sotamodel 的 10000 漏过去，
/// 又变成「显示一个花多少都不变的假余额」。
///
/// 判错的两个方向都有出路且不静默：
/// - 真有 ≥1 万额度的 token 被误判 → 用户看到的是**可行动的失败文案**（含上限原值），
///   照着填一次「取值路径」即可；
/// - 哨兵被漏判 → 才是真正糟的那种（一个用户会当真的错数字）。
///
/// 故这条线宁可偏严。
const BILLING_LIMIT_SENTINEL_USD: f64 = 10_000.0;

/// 本次返回是不是「只有配额上限、且那个上限是不限额哨兵」的形态。
///
/// 返回 `Some(limit)` = 是哨兵、拿不到余额 → 调用方报可行动的失败。
/// 返回 `None` 有两种情形：上游给了更精确的余额字段（与本函数无关），
/// 或上限在合理量级（限额 token）→ 调用方走 `上限 − 已用` 得真实剩余。
///
/// ## 两次真机：哨兵会给出用户会当真的错数字
///
/// - **sotamodel.net**：`hard_limit_usd = 10000` → 卡片显示「10000 USD」，花多少都不变；
/// - **agentrouter.org**：`hard_limit_usd = 100000000`（1e8）
///   → `1e8 − 1179.52 = 99,998,820.48 USD`，而站点网页上的真实余额是 **616.48 USD**。
///   注意 `1179.52` 恰好等于网页上的「历史消耗」——**减法是对的，被减数没有意义**
///   （`1179.52 + 616.48 = 1796`，与 1e8 差 5 个数量级）。
///
/// ## 但不能因此把整条路砍掉
///
/// 对**限额 token**，`hard_limit_usd` 就是这条 token 的额度，`上限 − 已用` = 真实剩余，
/// 且**零配置**就能算对。上一版把 `hard_limit_usd` 从候选链里整条删掉，
/// 顺带也让那些站查不出余额了 —— 那是修过头。故改为只挡哨兵，量级判据见
/// [`BILLING_LIMIT_SENTINEL_USD`]。
///
/// 判据是「**余额取自** `hard_limit_usd`」而不是「响应里有 `hard_limit_usd`」：
/// 若上游同时给了更精确的字段（候选链里排在前面的都算），那个值本身就是余额，与本函数无关。
/// 用户显式填了取值路径时一律尊重用户，不做任何自动干预。
fn billing_limit_is_not_a_balance(body: &Value, custom_path: Option<&str>) -> Option<f64> {
    let limit = billing_limit_only(body, custom_path)?;
    (limit >= BILLING_LIMIT_SENTINEL_USD).then_some(limit)
}

/// 「响应里只有配额上限、没有更精确的余额字段」——不判量级。
///
/// 与 [`billing_limit_is_not_a_balance`] 分开：那个只回答「是不是哨兵」，
/// 这个回答「要不要走 `上限 − 已用`」。两个问题共用同一段前置判断，抽出来免得写两遍走岔。
fn billing_limit_only(body: &Value, custom_path: Option<&str>) -> Option<f64> {
    if custom_path.map(str::trim).is_some_and(|s| !s.is_empty()) {
        return None;
    }
    let limit = find_number(body, &["hard_limit_usd", "data.hard_limit_usd"])?;
    // 候选链里有任何字段命中 → 那才是余额，`hard_limit_usd` 不参与（它已不在链里）。
    if derive_openrouter_remaining(body)
        .or_else(|| find_number_scaled(body, REMAINING_CANDIDATES))
        .is_some()
    {
        return None;
    }
    Some(limit)
}

/// 对单个 Key 执行一次余额查询。
///
/// `secret` 由调用方从密钥库取出（本模块不碰密钥库，保持可测）。
///
/// **可能按序试多个端点**：用户没填地址、域名又认不出时走
/// [`FALLBACK_ENDPOINTS`] 探测，命中即停并把命中的模板放进
/// `resolved_url_template` 供调用方写回配置（此后每次只发 1 个请求）。
/// 单端点场景（用户填了 / 域名认得出）行为与从前完全一致。
pub async fn query_balance(
    key: &ProviderKey,
    cfg: &BalanceQuery,
    secret: &str,
) -> BalanceResult {
    if !cfg.enabled {
        return BalanceResult::failed("未启用余额查询");
    }

    let base = cfg
        .base_url_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&key.base_url);

    let candidates = resolve_endpoint_candidates(cfg, base);
    let probing = candidates.len() > 1;
    let mut first_err: Option<BalanceResult> = None;
    // 「有信息量」的失败：它解释了**为什么**查不到（目前只有「只给了配额上限」这一种）。
    // 优先它而不是第一条 —— 否则最终报告是第一个候选那句 404，而真正的原因被丢了。
    let mut informative_err: Option<BalanceResult> = None;

    for (url_template, auth_scheme) in &candidates {
        let mut r = query_one_endpoint(key, cfg, secret, base, url_template, auth_scheme).await;
        if r.ok {
            // 只在真探测过时才回写：单端点场景没什么可「记住」的，回写等于把
            // 自动识别的结果固化成用户配置，日后我们改进识别表就再也不生效了。
            if probing {
                r.resolved_url_template = Some((*url_template).to_string());
            }
            return r;
        }
        // 瞬时失败（并发去重哨兵）不代表端点不对，也不该继续打其余端点。
        if r.transient {
            return r;
        }
        if informative_err.is_none()
            && r.error.as_deref().is_some_and(failure_explains_root_cause)
        {
            informative_err = Some(r.clone());
        }
        if first_err.is_none() {
            first_err = Some(r);
        }
    }

    // 全部候选都失败：优先报**解释了原因**的那条，其次报第一条（最可能对的那个端点）。
    // 只报最后一条会让用户去查 `hard_limit_usd` 那个最不可能的路径。
    let mut out = informative_err
        .or(first_err)
        .unwrap_or_else(|| BalanceResult::failed("没有可用的查询端点"));
    if probing {
        let tried: Vec<&str> = candidates.iter().map(|(u, _)| *u).collect();
        let note = format!("\n（已自动尝试 {} 个常见端点均失败：{}。若站点的余额接口不在其中，请在「查询地址」里手填。）", tried.len(), tried.join("、"));
        out.error = Some(match out.error.take() {
            Some(e) => format!("{e}{note}"),
            None => note.trim_start().to_string(),
        });
    }
    out
}

/// 对**一个**端点执行查询（[`query_balance`] 的单次尝试）。
async fn query_one_endpoint(
    key: &ProviderKey,
    cfg: &BalanceQuery,
    secret: &str,
    base: &str,
    url_template: &str,
    auth_scheme: &str,
) -> BalanceResult {

    // NewAPI 类面板用 accessToken + userId 而非 API Key，故一并展开。
    // 两者留空时展开成空串，模板里没用到就无影响。
    let access_token = cfg.access_token.as_deref().unwrap_or("");
    let user_id = cfg.user_id.as_deref().unwrap_or("");
    let url = expand_placeholders(url_template, base, secret, access_token, user_id);
    // 占位符展开后仍不是绝对 URL 说明配置有问题，早报错好过让 reqwest 抛一句晦涩的解析错。
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return BalanceResult::failed(format!("查询地址不是合法的 http(s) URL: {url}"));
    }

    let timeout = Duration::from_secs(if cfg.timeout_secs == 0 {
        DEFAULT_BALANCE_TIMEOUT_SECS as u64
    } else {
        cfg.timeout_secs as u64
    });

    // 用带自动解压的客户端：本函数拿到 body 后自己 resp.text()+from_str 解析，而中转站/CDN
    // 可能主动返回 gzip/br 压缩体（实测「余额查询返回一堆乱码字节」正是不解压所致）。
    // **不能用 shared_client()**——那是转发路径的字节透明客户端，不解压。
    let client = crate::upstream::decoding_client();
    let method = cfg.method.trim().to_ascii_uppercase();
    let mut req = match method.as_str() {
        "" | "GET" => client.get(&url),
        "POST" => client.post(&url),
        other => return BalanceResult::failed(format!("不支持的请求方法: {other}")),
    };
    req = req.timeout(timeout);

    // 认证头。四种形态：
    //   bearer / x-api-key —— 用转发密钥，与转发路径同口径
    //   access-token       —— NewAPI 类面板：认的是面板登录态，不是 API Key
    //   none               —— 密钥已在 URL 里的站点
    req = match auth_scheme.to_ascii_lowercase().as_str() {
        "" | "bearer" => req.bearer_auth(secret),
        // x-api-key 是官方 Anthropic 的认证形态，其端点（如 /v1/organizations/me）
        // **强制要求 anthropic-version 头**，缺了直接 400。自动识别把 api.anthropic.com
        // 映射到这个认证方式，故必须一并带上版本头——否则本轮新增的官方 Anthropic
        // 自动识别端点必然失败。版本号与转发路径同口径（apply_auth 里也用这个）。
        "x-api-key" => req
            .header("x-api-key", secret)
            .header("anthropic-version", "2023-06-01"),
        "access-token" => {
            if access_token.is_empty() {
                return BalanceResult::failed("该认证方式需要填 Access Token");
            }
            // NewAPI 要 Bearer + New-Api-User 两个头同时在（对齐 cc-switch 的 NewAPI 模板）。
            // 少了 New-Api-User 会被当成未指定用户，返回的不是你的额度。
            req.bearer_auth(access_token)
                .header("New-Api-User", user_id)
                .header("content-type", "application/json")
        }
        "none" => req,
        other => return BalanceResult::failed(format!("不支持的认证方式: {other}")),
    };

    // 客户端身份头（UA 等）。**这一步不能省**：本请求是应用自建的，
    // 而 `shared_client()` 不设默认 UA（reqwest 默认不发），于是部分中转渠道会把它
    // 判为 `detected: unknown` 直接 403（channel:client_restricted）——
    // 表现为「cc-switch 能查出余额、SynaRoute 查不出」，而两边配置一模一样。
    //
    // 这个坑本项目在聚合调用上已经踩过一次（见 `apply_client_identity` 的文档），
    // 余额查询是第二次。凡自建上游请求都要过那一道。
    req = crate::upstream::apply_client_identity(req, key.protocol);

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return BalanceResult::failed(format!("查询超时（{}s）", timeout.as_secs()))
        }
        Err(e) => return BalanceResult::failed(format!("请求失败: {e}")),
    };

    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return BalanceResult::failed(format!("读取响应失败: {e}")),
    };

    if !status.is_success() {
        // 带上状态码与截断的响应体：401/403 是配错密钥，404 是路径不对，
        // 不给这个区分用户只能盲猜。
        let snippet: String = text.chars().take(200).collect();
        return BalanceResult::failed(format!("上游返回 HTTP {status}: {snippet}"));
    }

    match serde_json::from_str::<Value>(&text) {
        Ok(v) => {
            let mut result = extract_balance(&v, cfg.remaining_path.as_deref());
            // 只拿到「配额上限」而没有余额：如实报失败并指路，**不拿 `上限 − 已用` 充当余额**。
            // 两次真机都证明那个数字是错的（10000 / 99,998,820.48 vs 真实 616.48），
            // 完整理由见 `billing_limit_is_not_a_balance`。
            // 顺手把配对端点的「已用」读出来放进文案 —— 那个数是真的，对用户有用；
            // 再顺手探一次 `/api/status`，认出 NewAPI 站就能给出**精确**的出路而不是泛泛指引。
            if let Some(limit) = billing_limit_is_not_a_balance(&v, cfg.remaining_path.as_deref()) {
                let used = read_billing_usage_usd(
                    &client, &url, secret, auth_scheme, access_token, user_id, timeout,
                    key.protocol,
                )
                .await;
                let is_newapi =
                    read_newapi_quota_unit(&client, &origin_of(base), timeout).await.is_some();
                return BalanceResult::failed(billing_limit_reason(limit, used, is_newapi));
            }
            // **限额 token**：`hard_limit_usd` 就是这条 token 的额度，`上限 − 已用` = 真实剩余，
            // 而且**零配置**就能算对。上一版把这条路整条砍了（连带那些本来查得出的站也废了），
            // 是修过头 —— 现在只挡哨兵（见 BILLING_LIMIT_SENTINEL_USD），其余照算。
            if let Some(limit) = billing_limit_only(&v, cfg.remaining_path.as_deref()) {
                match read_billing_usage_usd(
                    &client, &url, secret, auth_scheme, access_token, user_id, timeout,
                    key.protocol,
                )
                .await
                {
                    Some(used) => {
                        // 负数夹到 0：上游两个数不同源时（用量口径与上限不一致）显示负余额
                        // 只会让人困惑，而 0 是「已用完」这个结论的正确表达。
                        result.remaining = Some((limit - used).max(0.0));
                        result.total = Some(limit);
                        result.used = Some(used);
                        result.unit = Some(result.unit.take().unwrap_or_else(|| "USD".into()));
                        result.ok = true;
                        result.error = None;
                    }
                    // 读不到已用就只有上限，那不是余额 —— 如实报，别把上限当余额显示。
                    None => {
                        return BalanceResult::failed(format!(
                            "该站点只给了配额上限（{BILLING_LIMIT_MARKER}{limit}），\
                             而配对的已用额度接口读不到，无法算出剩余。\n\
                             直接显示上限会让人误以为那是余额，故不显示。可稍后重试，\
                             或在「查询地址」填这个站点真实的余额接口。"
                        ));
                    }
                }
            }
            // NewAPI 的内部计费单位 → 按站点公开的 `quota_per_unit` 换成钱。
            // 不缩放会把 616.48 USD 显示成 308240000（与上面那条同类的错数字）；
            // 硬编码 500000 又会在改过比率的实例上错 —— 故现场问站点要。
            if let Some(raw) = newapi_quota_needs_scaling(&v, cfg.remaining_path.as_deref()) {
                match read_newapi_quota_unit(&client, &origin_of(base), timeout).await {
                    Some((per_unit, unit)) => {
                        result.remaining = Some(raw / per_unit);
                        // 站点自报的显示单位优先（sotamodel 给 "USD"）；没给就沿用已解析的。
                        if let Some(u) = unit {
                            result.unit = Some(u);
                        }
                        // total / used 若也取自同一套内部单位，一并换算，否则百分比会离谱。
                        result.total = result.total.map(|t| t / per_unit);
                        result.used = result.used.map(|u| u / per_unit);
                    }
                    // 读不到比率就**不猜**：宁可如实说，也不给一个可能差 50 万倍的数字。
                    None => {
                        return BalanceResult::failed(format!(
                            "该站点的余额是 NewAPI 的{NEWAPI_QUOTA_MARKER}{raw}），\
                             而换算比率要从它的 `{NEWAPI_STATUS_PATH}` 读，这次读不到。\n\
                             直接显示 {raw} 会差出几十万倍，故不显示。\
                             可在「取值路径」手填已是货币单位的字段，或稍后重试。"
                        ));
                    }
                }
            }
            result
        }
        Err(e) => {
            // HTML 响应单独给一条**可照做**的提示，不要只丢一句「不是合法 JSON」。
            //
            // 拿到 HTML 意味着这个 URL 是个**网页**而不是 API —— 十有八九是路径填错、
            // 打到了站点首页或登录页。而原来的文案把 `<!doctype html>` 直接糊在错误里，
            // 用户看到的是一堆 `<meta charset=...>`，完全不知道该改哪里
            // （真机反馈：用户以为是程序坏了）。
            if looks_like_html(&text) {
                return BalanceResult::failed(format!(
                    "该地址返回的是网页而不是 API 数据（{url}）。\
                     通常是查询地址填错了：\n\
                     • 确认站点的余额接口路径（常见为 /user/balance、/api/user/self、/v1/dashboard/billing/subscription）\n\
                     • baseUrl 带路径后缀时（如 …/anthropic）请把模板里的 {{{{baseUrl}}}} 换成 {{{{origin}}}}\n\
                     • 站点若只有网页版账单、没有开放接口，则无法查询余额"
                ));
            }
            let snippet: String = text.chars().take(200).collect();
            BalanceResult::failed(format!("响应不是合法 JSON（{e}）: {snippet}"))
        }
    }
}

/// NewAPI / OneAPI 系的**公开**状态接口（无需任何认证）。
///
/// 它给出两件我们必需的东西（2026-08-22 实测 agentrouter.org 与 sotamodel.net）：
/// - `data.quota_per_unit`：内部计费单位与货币的换算比率，两站都是 **500000**（= 1 USD）；
/// - `data.quota_display_type`：显示单位（sotamodel 给 `"USD"`）。
///
/// **它同时是一个免费的站点指纹**：能读到 `quota_per_unit` 就说明这是 NewAPI 系，
/// 于是我们能精确地告诉用户「余额在 `/api/user/self`，而它要面板 Access Token」——
/// 而不是让他从一句「找不到余额字段」里猜。
const NEWAPI_STATUS_PATH: &str = "/api/status";

/// 本次返回的余额是否取自 NewAPI 的**内部计费单位**字段（`quota` / `data.quota`）。
///
/// 返回 `Some(原始值)` 表示需要按 `quota_per_unit` 缩放才能变成钱。
///
/// ## 为什么必须缩放（而不是像从前那样「刻意不缩放」）
///
/// 旧注释的顾虑是「硬编码 500000 对改过比率的实例会错」。这个顾虑对，但结论错了 ——
/// 正确做法不是放弃缩放，而是**去问站点要比率**（`/api/status` 公开、免认证）。
/// 不缩放的后果实测是把 616.48 USD 显示成 `308240000`，与 `hard_limit_usd` 那条
/// 属同一类「给用户一个会当真的错数字」。
///
/// 判据同 [`billing_limit_is_not_a_balance`]：只在「余额**确实取自**该字段」时成立；
/// 用户显式填了取值路径时一律尊重用户，不做任何自动干预。
fn newapi_quota_needs_scaling(body: &Value, custom_path: Option<&str>) -> Option<f64> {
    if custom_path.map(str::trim).is_some_and(|s| !s.is_empty()) {
        return None;
    }
    let quota = find_number(body, &["data.quota", "quota"])?;
    // 候选链取到的值等于 quota → 说明前面更精确（已是货币）的字段都没命中。
    // 用 f64 相等比较是安全的：两者取自**同一个** JSON 数字，没有中间运算。
    let hit = derive_openrouter_remaining(body)
        .or_else(|| find_number_scaled(body, REMAINING_CANDIDATES))?;
    (hit == quota).then_some(quota)
}

/// 读站点的 `quota_per_unit` 与显示单位（公开接口，不带任何认证）。
///
/// `origin` 是站点根（`https://x.com`）。返回 `(每单位货币对应的 quota, 显示单位)`。
/// 读不到就返回 `None` —— 调用方**不许猜一个 500000 顶上**：那正是旧实现被否掉的做法。
async fn read_newapi_quota_unit(
    client: &reqwest::Client,
    origin: &str,
    timeout: Duration,
) -> Option<(f64, Option<String>)> {
    let url = format!("{}{NEWAPI_STATUS_PATH}", origin.trim_end_matches('/'));
    let resp = client.get(&url).timeout(timeout).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = serde_json::from_str(&resp.text().await.ok()?).ok()?;
    let per_unit = find_number(&v, &["data.quota_per_unit", "quota_per_unit"])?;
    // 比率必须是正数：0 或负数会让除法产出 inf / 负余额（本项目已被 `"inf"` 咬过一次）。
    if !(per_unit.is_finite() && per_unit > 0.0) {
        return None;
    }
    let unit = find_string(&v, &["data.quota_display_type", "quota_display_type"]);
    Some((per_unit, unit))
}

/// 「只拿到配额上限」这条失败的标识串。
///
/// 生产方 [`billing_limit_reason`] 与消费方（[`failure_explains_root_cause`]）**共用这一个常量**，
/// 避免两处各写一遍字面量而漂移。
const BILLING_LIMIT_MARKER: &str = "hard_limit_usd = ";

/// 「拿到的是 NewAPI 内部计费单位、但读不到换算比率」这条失败的标识串。
const NEWAPI_QUOTA_MARKER: &str = "内部计费单位（quota = ";

/// 这条失败**解释了根因**吗（而不只是「404 / 找不到字段」）。
///
/// 探测链会按序试几个端点，最终只能报一条。默认报第一条（最可能对的那个端点），
/// 但若某条失败其实**说清了为什么查不到**，那条才是用户需要看的 ——
/// 否则界面上是一句干巴巴的 `404 not found`，而真正的原因（「这站的余额要面板 Access Token」
/// 或「拿到的是内部计费单位、比率读不到」）被丢在了后面的候选里。
///
/// 判据集中在这里、由各生产方共用常量，是为了不让「谁算有信息量」散落成一串字面量比较。
fn failure_explains_root_cause(err: &str) -> bool {
    err.contains(BILLING_LIMIT_MARKER) || err.contains(NEWAPI_QUOTA_MARKER)
}

/// 「只拿到配额上限」时的失败文案。
///
/// 必须可行动，且必须把**已读到的真实信息**说出来：`used` 来自配对的 usage 端点，
/// 它与站点网页上的「历史消耗」实测逐位吻合（agentrouter.org：1179.52），对用户有用。
/// 而 `limit` 要原样报出来 —— 用户看到 `100000000` 自己就明白那是「无限额」而非余额。
///
/// `is_newapi` = 站点的公开 `/api/status` 认出来了（见 [`NEWAPI_STATUS_PATH`]）。
/// 认出来时给的是**精确到字段**的操作步骤，而不是「请填真实的余额接口」这种等于没说的话：
/// 实测 agentrouter.org 的 `/api/user/self` 明确回「access token 无效」——
/// 转发用的 API Key 就是拿不到余额，只有面板 Access Token 行。这一步不告诉用户，
/// 他会一直以为是我们没查对。
fn billing_limit_reason(limit: f64, used: Option<f64>, is_newapi: bool) -> String {
    let used_part = match used {
        Some(u) => format!("已用约 {u:.2} USD（这个数是准的，与站点「历史消耗」同源）。"),
        None => String::new(),
    };
    let head = format!(
        "该站点的计费接口只给「配额上限」（{BILLING_LIMIT_MARKER}{limit}），没有余额字段。\
         {used_part}\n\
         上限不是余额：{limit} 这类数字在 NewAPI 系里通常表示「无限额」，\
         拿它减掉已用得到的不是你的余额（实测会差出几个数量级），故这里不显示。\n"
    );
    if is_newapi {
        format!(
            "{head}\
             已认出这是 **NewAPI 架构**的站点。它的余额在 `/api/user/self`，\
             而那个接口**只认面板 Access Token、不认转发用的 API Key**（实测会回\
             「access token 无效」）。按下面四步填一次即可：\n\
             ① 到站点网页 → 个人设置 → 生成 **Access Token**；\n\
             ② 在本页展开「凭证覆盖」，把它填进 **Access Token**；\n\
             ③ **用户 ID** 填站点网页上你的 ID（个人设置页顶部那个数字）；\n\
             ④ **认证方式**选 `access-token`，**查询地址留空**（会自动探到 /api/user/self）。\n\
             填好后点「测试查询」—— 内部计费单位会按站点公开的 quota_per_unit 自动换算成钱。"
        )
    } else {
        format!(
            "{head}\
             请在「查询地址」里填这个站点真实的余额接口，或在「取值路径」指定余额字段；\
             若站点只有网页版账单、没有开放接口，则无法查询余额。"
        )
    }
}

/// 读配对的 usage 端点拿「已用美元」；读不到返回 `None`（纯信息用途，不影响成败判定）。
///
/// usage 端点与 subscription 同源同前缀，直接替换尾部路径、**不重新推导 origin**
/// （用户可能填了自定义 `base_url_override`，重新推导会打到别的域名去）。
#[allow(clippy::too_many_arguments)]
async fn read_billing_usage_usd(
    client: &reqwest::Client,
    subscription_url: &str,
    secret: &str,
    auth_scheme: &str,
    access_token: &str,
    user_id: &str,
    timeout: Duration,
    protocol: crate::model::Protocol,
) -> Option<f64> {
    let usage_url = subscription_url
        .rfind("/v1/dashboard/billing/subscription")
        .map(|pos| format!("{}{}", &subscription_url[..pos], BILLING_USAGE_PATH))?;

    let mut req = client.get(&usage_url).timeout(timeout);
    // 认证与主请求完全一致：同一套 billing API，鉴权形态不会变。
    req = match auth_scheme.to_ascii_lowercase().as_str() {
        "" | "bearer" => req.bearer_auth(secret),
        "x-api-key" => req
            .header("x-api-key", secret)
            .header("anthropic-version", "2023-06-01"),
        "access-token" => req
            .bearer_auth(access_token)
            .header("New-Api-User", user_id)
            .header("content-type", "application/json"),
        _ => req,
    };
    req = crate::upstream::apply_client_identity(req, protocol);
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    serde_json::from_str::<Value>(&text).ok().and_then(|v| parse_billing_usage_usd(&v))
}

/// 响应体是否是 HTML 而非 JSON。
///
/// 判据刻意宽松（前 200 字符里出现任一 HTML 起始标记即算）：这里只用来**换一条更好的
/// 错误文案**，判错的代价仅是提示措辞略偏，而判据严格反而会让「带 BOM / 前置空行 /
/// 以注释开头」的真实 HTML 漏出去、退回那句没用的「不是合法 JSON」。
fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start().chars().take(200).collect::<String>().to_ascii_lowercase();
    head.starts_with("<!doctype")
        || head.starts_with("<html")
        || head.starts_with("<head")
        || head.starts_with("<?xml")
        || head.contains("<meta charset")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起一个**按路径分流**的 mock 上游，返回 `http://127.0.0.1:port`。
    ///
    /// `routes` 是 `(路径, 状态码, 响应体)`；未列出的路径一律 404。
    /// 按路径分流是必需的：探测链要验的就是「哪个路径给出了正确答案」，
    /// 一个恒定响应的 mock 无法区分「试对了」和「随便打哪都行」。
    async fn spawn_path_mock(routes: &'static [(&'static str, u16, &'static str)]) -> String {
        use hyper::service::service_fn;
        use hyper::{Request, Response};
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            0,
        )))
        .await
        .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<hyper::body::Incoming>| async move {
                        let path = req.uri().path().to_string();
                        let hit = routes.iter().find(|(p, _, _)| *p == path);
                        let (status, body) = match hit {
                            Some((_, s, b)) => (*s, *b),
                            None => (404, r#"{"error":"not found"}"#),
                        };
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                                .unwrap(),
                        )
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// 端到端：认不出的中转站按序探测，拿到 `/v1/usage` 的**真实余额**而不是 10000 上限。
    ///
    /// 这条直接复现并锁死真机缺陷：站点同时提供
    /// - `/v1/usage` → 真实剩余 `{"remaining": 42.5, "unit": "USD"}`
    /// - `/v1/dashboard/billing/subscription` → `{"hard_limit_usd": 10000}`（配额上限）
    ///
    /// 原实现兜底只打后者 → 用户永远看到「10000 USD」，且那个数字花多少都不动。
    /// 探测链把 `/v1/usage` 排在前面，于是拿到 42.5。
    ///
    /// 同时钉住「记住命中的那条」：`resolved_url_template` 必须是 `/v1/usage` ——
    /// 没有它，每次自动轮询都要重跑整条探测，对按请求限流的站点是实打实的浪费。
    #[tokio::test]
    async fn probing_prefers_real_balance_over_quota_ceiling() {
        static ROUTES: &[(&str, u16, &str)] = &[
            ("/v1/usage", 200, r#"{"remaining": 42.5, "unit": "USD"}"#),
            (
                "/v1/dashboard/billing/subscription",
                200,
                r#"{"object":"billing_subscription","hard_limit_usd":10000}"#,
            ),
        ];
        let base = spawn_path_mock(ROUTES).await;
        let key = ProviderKey { base_url: base.clone(), ..Default::default() };
        let cfg = BalanceQuery { enabled: true, ..Default::default() };

        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(r.ok, "应从 /v1/usage 拿到余额，实际失败：{:?}", r.error);
        assert_eq!(
            r.remaining,
            Some(42.5),
            "必须是 /v1/usage 的真实剩余，不是 billing 端点那个恒为 10000 的配额上限"
        );
        assert_eq!(
            r.resolved_url_template.as_deref(),
            Some("{{origin}}/v1/usage"),
            "命中的端点必须被记住，否则每次轮询都要重跑整条探测"
        );
    }

    /// **限额 token 的余额必须零配置就算出来**（`上限 − 已用`），只有哨兵才拒。
    ///
    /// 这条钉的是一次「修过头」：上一版为了挡住 agentrouter 的 1e8 哨兵，把
    /// `hard_limit_usd` 从候选链里**整条删掉**，顺带让那些本来零配置就查得出余额的站
    /// 也全查不出了。用户的原话是「cc-switch 都不用用户操作」——凡是能自动算对的，
    /// 就不该退化成「请手填」。
    ///
    /// 两侧都钉，缺一侧都会让这个平衡塌向一边：
    /// 1. 上限在合理量级（20 USD）+ 已用 5 USD → **零配置**得出 15 USD；
    /// 2. 上限是哨兵（1e8 / 10000）→ 拒绝出数字，给可行动文案；
    /// 3. 读不到「已用」→ 也拒绝（只有上限不是余额）；
    /// 4. 上游给了更精确的字段 → 那条优先，不走减法。
    #[tokio::test]
    async fn limited_token_balance_is_derived_with_zero_config() {
        let cfg = BalanceQuery { enabled: true, ..Default::default() };

        // ① 限额 token：零配置算出 15 USD
        static LIMITED: &[(&str, u16, &str)] = &[
            (
                "/v1/dashboard/billing/subscription",
                200,
                r#"{"object":"billing_subscription","hard_limit_usd":20,"soft_limit_usd":20}"#,
            ),
            ("/v1/dashboard/billing/usage", 200, r#"{"object":"list","total_usage":500}"#),
        ];
        let base = spawn_path_mock(LIMITED).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(r.ok, "限额 token 必须零配置算出余额，不该退化成「请手填」：{:?}", r.error);
        assert_eq!(r.remaining, Some(15.0), "20 − 5 = 15（total_usage 是美分）");
        assert_eq!(r.total, Some(20.0));
        assert_eq!(r.used, Some(5.0));

        // ② 哨兵：拒绝出数字（两个已知哨兵都在阈值之上）
        for sentinel in [100_000_000.0_f64, 10_000.0] {
            let body = serde_json::json!({
                "object": "billing_subscription", "hard_limit_usd": sentinel
            });
            assert_eq!(
                billing_limit_is_not_a_balance(&body, None),
                Some(sentinel),
                "{sentinel} 必须判为哨兵 —— 否则又变成显示一个花多少都不变的假余额"
            );
        }
        // 阈值下方一点必须**不**判哨兵（否则限额 token 全被误伤）
        let ok_limit = serde_json::json!({ "hard_limit_usd": 9_999.0 });
        assert!(billing_limit_is_not_a_balance(&ok_limit, None).is_none());
        assert_eq!(billing_limit_only(&ok_limit, None), Some(9_999.0));

        // ③ 读不到「已用」→ 拒绝（只有上限不是余额）
        static NO_USAGE: &[(&str, u16, &str)] = &[(
            "/v1/dashboard/billing/subscription",
            200,
            r#"{"object":"billing_subscription","hard_limit_usd":20}"#,
        )];
        let base = spawn_path_mock(NO_USAGE).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(!r.ok, "只有上限、拿不到已用时不得把上限当余额显示");
        assert!(r.error.unwrap_or_default().contains("20"), "要报出上限原值");

        // ④ 更精确的字段优先，不走减法
        let precise = serde_json::json!({ "remaining": 3.25, "hard_limit_usd": 20 });
        assert!(billing_limit_only(&precise, None).is_none());
        assert_eq!(extract_balance(&precise, None).remaining, Some(3.25));
    }

    /// 端到端复现真机那条错数字，并锁死它不再出现。
    ///
    /// agentrouter.org 的实际形态（2026-08-22 真机截图）：
    /// - `/v1/usage`、`/api/user/self` 都拿不到余额；
    /// - `/v1/dashboard/billing/subscription` → `hard_limit_usd = 100000000`；
    /// - `/v1/dashboard/billing/usage` → `total_usage = 117952`（美分）= 1179.52 USD，
    ///   与站点网页的「历史消耗 $1179.52」逐位吻合。
    ///
    /// 上一版据此算出 **99,998,820.48 USD** 并显示成「查询成功」，而网页上的真实余额是
    /// **616.48 USD**。这条钉三件事：
    /// 1. **不得**再报成功、不得出现那个数字；
    /// 2. 失败文案要带上限原值与已用（后者是准的，对用户有用）；
    /// 3. 挑出来报告的是这条**有信息量**的失败，而不是第一个候选那句干巴巴的 404。
    #[tokio::test]
    async fn agentrouter_quota_ceiling_is_reported_as_failure_not_as_balance() {
        static ROUTES: &[(&str, u16, &str)] = &[
            (
                "/v1/dashboard/billing/subscription",
                200,
                r#"{"object":"billing_subscription","hard_limit_usd":100000000,"soft_limit_usd":100000000}"#,
            ),
            ("/v1/dashboard/billing/usage", 200, r#"{"object":"list","total_usage":117952}"#),
        ];
        let base = spawn_path_mock(ROUTES).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let cfg = BalanceQuery { enabled: true, ..Default::default() };

        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(!r.ok, "只有配额上限时不得报成功");
        let err = r.error.unwrap_or_default();
        assert!(
            !err.contains("99998820") && !err.contains("99,998,820"),
            "绝不能再出现那个错数字：{err}"
        );
        assert!(err.contains("100000000"), "要把上限原值报出来供用户判断：{err}");
        assert!(err.contains("1179.52"), "已用是准的（= 网页「历史消耗」），要说出来：{err}");
        assert!(err.contains("查询地址"), "要给出可行动的出路：{err}");
        assert!(
            r.resolved_url_template.is_none(),
            "失败不得记住端点，否则下次连探测的机会都没了"
        );
    }

    /// **NewAPI 内部计费单位必须换成钱** —— 这条直接钉住真机那个 616.48。
    ///
    /// ## 取证（2026-08-22，拿用户 cc-switch 库里的明文 token 实打）
    ///
    /// agentrouter.org 的实际行为：
    /// - `/v1/usage` → **404**（我们探测链的第一条对这个站无效）；
    /// - `/api/user/self` → `{"message":"无权进行此操作，access token 无效"}`
    ///   —— **转发用的 API Key 拿不到余额**，只有面板 Access Token 行；
    /// - `/api/status` → **公开、免认证**，返回 `data.quota_per_unit = 500000`
    ///   （sotamodel.net 同样是 500000，且另给 `quota_display_type = "USD"`）。
    ///
    /// 于是余额链闭合：`quota / quota_per_unit`。验算 `308240000 / 500000 = 616.48`，
    /// 与站点网页显示的「当前余额 $616.48」一致。
    ///
    /// ## 为什么不硬编码 500000
    ///
    /// 站长可以改 `QuotaPerUnit`。旧注释因此选择「刻意不缩放」，但那个结论是错的 ——
    /// 不缩放会把 616.48 显示成 `308240000`，与 `hard_limit_usd` 那条属同一类错数字。
    /// 正确做法是**去问站点要比率**（那个接口公开、免认证、零成本）。
    ///
    /// 四个方向一起钉：
    /// 1. 能读到比率 → 换算出 616.48，单位取站点自报的；
    /// 2. 读**不**到比率 → 报失败，**不许**猜 500000 顶上（差 50 万倍）；
    /// 3. 比率 ≤ 0 或非有限 → 当作读不到（否则除出 inf / 负余额，本仓被 `"inf"` 咬过一次）；
    /// 4. 上游给的已是货币字段（`remaining` 等）→ 不走缩放，原样用。
    #[tokio::test]
    async fn newapi_internal_quota_is_scaled_into_money() {
        // ① 正常：quota + 公开的 quota_per_unit → 616.48
        static OK_ROUTES: &[(&str, u16, &str)] = &[
            ("/api/user/self", 200, r#"{"success":true,"data":{"quota":308240000,"username":"x"}}"#),
            (
                "/api/status",
                200,
                r#"{"data":{"quota_per_unit":500000,"quota_display_type":"USD","price":7.3}}"#,
            ),
        ];
        let base = spawn_path_mock(OK_ROUTES).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let cfg = BalanceQuery { enabled: true, ..Default::default() };
        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(r.ok, "应换算成功：{:?}", r.error);
        assert_eq!(
            r.remaining,
            Some(616.48),
            "308240000 / 500000 = 616.48，与站点网页显示的当前余额一致；\
             不缩放会显示成 308240000（差 50 万倍）"
        );
        assert_eq!(r.unit.as_deref(), Some("USD"), "单位取站点自报的 quota_display_type");

        // ② 读不到比率 → 报失败，绝不猜 500000
        static NO_STATUS: &[(&str, u16, &str)] = &[(
            "/api/user/self",
            200,
            r#"{"success":true,"data":{"quota":308240000}}"#,
        )];
        let base = spawn_path_mock(NO_STATUS).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(!r.ok, "拿不到换算比率时不得出数字");
        let err = r.error.unwrap_or_default();
        assert!(err.contains("308240000"), "要把原始值报出来供用户判断：{err}");
        assert!(err.contains(NEWAPI_STATUS_PATH), "要说清比率该从哪读：{err}");

        // ③ 比率非法（0）→ 同样当读不到，不得除出 inf
        static BAD_UNIT: &[(&str, u16, &str)] = &[
            ("/api/user/self", 200, r#"{"success":true,"data":{"quota":100}}"#),
            ("/api/status", 200, r#"{"data":{"quota_per_unit":0}}"#),
        ];
        let base = spawn_path_mock(BAD_UNIT).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(!r.ok, "比率为 0 时不得产出 inf 余额");

        // ④ 上游给的已是货币字段 → 不走缩放
        let money = serde_json::json!({ "data": { "remaining": 12.5, "quota": 6_250_000 } });
        assert!(
            newapi_quota_needs_scaling(&money, None).is_none(),
            "remaining 已是货币，不该再按 quota 缩放"
        );
        assert_eq!(extract_balance(&money, None).remaining, Some(12.5));
    }

    /// 认出 NewAPI 站时，「只有配额上限」的失败必须给**精确到字段**的四步操作。
    ///
    /// 泛泛一句「请填真实的余额接口」等于没说 —— 用户根本不知道 agentrouter 的余额
    /// 藏在 `/api/user/self` 后面、且那个接口不认转发用的 API Key（实测回
    /// 「access token 无效」）。不告诉他，他会一直以为是我们没查对。
    #[test]
    fn newapi_site_gets_precise_four_step_instructions() {
        let newapi = billing_limit_reason(100_000_000.0, Some(1179.52), true);
        assert!(newapi.contains("NewAPI"), "要点明站点架构：{newapi}");
        assert!(newapi.contains("/api/user/self"), "要说出余额到底在哪个接口：{newapi}");
        assert!(
            newapi.contains("Access Token") && newapi.contains("用户 ID"),
            "要说清要填哪两样：{newapi}"
        );
        assert!(newapi.contains("access-token"), "要说清认证方式选哪个：{newapi}");
        assert!(newapi.contains("查询地址留空"), "留空才会自动探到 /api/user/self：{newapi}");

        // 认不出架构时不许编造这套步骤（那会把用户送去一个不存在的设置页）
        let generic = billing_limit_reason(10_000.0, None, false);
        assert!(!generic.contains("/api/user/self"), "认不出时不得编造具体接口：{generic}");
        assert!(generic.contains("查询地址"), "仍要给出泛化出路：{generic}");
    }

    /// 探测全部落空时：报**第一条**（最可能对的）的错，并列出试过哪些。
    ///
    /// 只报最后一条会把用户引向 `hard_limit_usd` 那个最不可能的路径 ——
    /// 而他真正该做的是去手填自己站点的地址。故错误信息里必须有「试过什么」+「去哪手填」。
    #[tokio::test]
    async fn all_probes_failing_reports_first_error_and_lists_attempts() {
        static NONE: &[(&str, u16, &str)] = &[]; // 全部 404
        let base = spawn_path_mock(NONE).await;
        let key = ProviderKey { base_url: base, ..Default::default() };
        let cfg = BalanceQuery { enabled: true, ..Default::default() };

        let r = query_balance(&key, &cfg, "sk-test").await;
        assert!(!r.ok);
        let err = r.error.unwrap_or_default();
        assert!(err.contains("/v1/usage"), "要指出第一条候选是什么：{err}");
        assert!(err.contains("已自动尝试"), "要说明试过多个端点：{err}");
        assert!(err.contains("手填"), "要给出可行动的出路：{err}");
        assert!(
            r.resolved_url_template.is_none(),
            "全失败时不得记住任何端点，否则下次连探测的机会都没了"
        );
    }

    /// 各家中转站的返回结构差异很大，候选链必须都能兜住 ——
    /// 兜不住就等于用户配了余额查询却永远显示「找不到字段」。
    #[test]
    fn extract_balance_handles_various_upstream_shapes() {
        // cc-switch 通用模板的三种形态
        let a = serde_json::json!({ "remaining": 84.2, "unit": "CNY", "is_active": true });
        let r = extract_balance(&a, None);
        assert!(r.ok);
        assert_eq!(r.remaining, Some(84.2));
        assert_eq!(r.unit.as_deref(), Some("CNY"));
        assert_eq!(r.is_valid, Some(true));

        let b = serde_json::json!({ "quota": { "remaining": 12.5, "unit": "USD" } });
        assert_eq!(extract_balance(&b, None).remaining, Some(12.5));

        let c = serde_json::json!({ "balance": 7 });
        let r = extract_balance(&c, None);
        assert_eq!(r.remaining, Some(7.0));
        assert_eq!(r.unit.as_deref(), Some("USD"), "取不到单位时默认 USD");

        // newapi / oneapi 系：余额在 data 下
        let d = serde_json::json!({ "data": { "balance": "36.80" } });
        assert_eq!(
            extract_balance(&d, None).remaining,
            Some(36.8),
            "字符串形态的余额必须能解析（不少站点为避免大数精度而用字符串）"
        );

        // 布尔的各种写法
        let e = serde_json::json!({ "balance": 1, "is_active": 0 });
        assert_eq!(extract_balance(&e, None).is_valid, Some(false));
        let f = serde_json::json!({ "balance": 1, "isValid": "true" });
        assert_eq!(extract_balance(&f, None).is_valid, Some(true));
    }

    /// DeepSeek 的**真实**返回结构（2026-08-13 实测原文），余额在数组里。
    ///
    /// 这条钉住两件事：
    ///   1. 候选链支持数组下标（`balance_infos.0.total_balance`）——
    ///      纯对象取值拿不到它，而这是一家主流厂商的实际结构；
    ///   2. 币种也从同一个数组元素里取（CNY，不是默认的 USD）——
    ///      默认 USD 而实际是人民币，会让用户把 4.28 元看成 4.28 美元。
    #[test]
    fn deepseek_real_response_shape() {
        let body = serde_json::json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "CNY",
                "total_balance": "4.28",
                "granted_balance": "0.00",
                "topped_up_balance": "4.28"
            }]
        });
        let r = extract_balance(&body, None);
        assert!(r.ok, "DeepSeek 结构必须能解析，实际错误: {:?}", r.error);
        assert_eq!(r.remaining, Some(4.28), "余额在 balance_infos[0].total_balance");
        assert_eq!(r.unit.as_deref(), Some("CNY"), "币种必须取实际值，不能默认成 USD");
        assert_eq!(r.is_valid, Some(true), "is_available 应被识别为有效性字段");
    }

    /// NewAPI / OpenAI 兼容计费层的真实返回结构（`/v1/dashboard/billing/subscription`）。
    ///
    /// **契约在 2026-08-22 改过两次**，这条测的是最终形态。
    ///
    /// 原契约：把 `hard_limit_usd` 直接当余额。两次真机证明对**哨兵**是错的
    /// （10000；99,998,820.48 vs 真实 616.48）。
    ///
    /// 第一次改：把它从候选链里整条删掉 —— **修过头**，连限额 token 那些本来
    /// 零配置就能算对的站也废了。
    ///
    /// 最终契约：`hard_limit_usd` 不进候选链（它不是余额），但
    /// - 量级合理（限额 token）→ 由调用方走 `上限 − 已用` 算真实剩余，**零配置**；
    /// - 量级达哨兵阈值 → 拒绝出数字，走可行动文案。
    ///
    /// 这条钉「候选链里确实没有它」+「10.5 这种合理量级不判哨兵」两半边；
    /// 完整方向见 `limited_token_balance_is_derived_with_zero_config` 与
    /// `agentrouter_quota_ceiling_is_reported_as_failure_not_as_balance`。
    #[test]
    fn newapi_billing_subscription_shape() {
        // NewAPI 的标准返回（对齐 OpenAI 的 billing_subscription 契约）
        let body = serde_json::json!({
            "object": "billing_subscription",
            "has_payment_method": false,
            "soft_limit_usd": 8.0,
            "hard_limit_usd": 10.5,
            "system_hard_limit_usd": 10.5,
            "access_until": 0
        });
        let r = extract_balance(&body, None);
        assert!(
            !r.ok && r.remaining.is_none(),
            "配额上限不得**直接**当余额产出（把 hard_limit_usd 加回候选链，本断言必红）：{r:?}"
        );
        assert!(
            billing_limit_is_not_a_balance(&body, None).is_none(),
            "10.5 是合理量级（限额 token），不该被判成哨兵 —— 否则这类站会退化成「请手填」"
        );
        assert_eq!(
            billing_limit_only(&body, None),
            Some(10.5),
            "但必须被识别为「只有上限」，调用方才会去减已用算出真实剩余"
        );

        // 更精确的字段存在时照常产出余额（本次改动不影响这条路径）
        let with_remaining = serde_json::json!({
            "object": "billing_subscription",
            "remaining": 3.25,
            "hard_limit_usd": 10.5
        });
        assert_eq!(
            extract_balance(&with_remaining, None).remaining,
            Some(3.25),
            "上游给了真实 remaining 就用它"
        );
    }

    /// **`hard_limit_usd` 是配额上限、根本不是余额** —— 必须报失败并指路，不得拿它出数字。
    ///
    /// ## 两次真机都给出了用户会当真的错数字
    ///
    /// - **sotamodel.net**：`hard_limit_usd = 10000`（NewAPI 默认）→ 卡片显示「10000 USD」，
    ///   花多少都不变；
    /// - **agentrouter.org**：`hard_limit_usd = 100000000`（1e8，NewAPI 的「无限额」哨兵）
    ///   → 上一版拿 `上限 − 已用` 算出 **99,998,820.48 USD**，而站点网页上的真实余额是
    ///   **616.48 USD**。那个减数 `1179.52` 恰好等于网页上的「历史消耗」——
    ///   **减法是对的，被减数没有意义**。
    ///
    /// 响应体里没有任何字段能区分「上限恰好等于预付金额」（相减有效）与「上限是哨兵」
    /// （相减无意义），既然区分不了就不能猜。故这条钉住：**返回 `Some(limit)` = 判为不可用**。
    ///
    /// 四个方向一起钉，少一条这个修复就会被后人「优化」回去：
    /// 1. 只有上限 → 识别出来（拿到 limit 原值，供文案原样报给用户）；
    /// 2. 有更精确的 `remaining` → **不**识别（那个值本身就是余额，与本函数无关）；
    /// 3. 用户显式填了取值路径 → 一律尊重用户，不做任何自动干预；
    /// 4. 文案必须可行动：报出上限原值 + 已用 + 去哪里填真实地址。
    #[test]
    fn billing_hard_limit_is_never_reported_as_balance() {
        // ① 只有上限 → 判为「不是余额」
        let limit_only = serde_json::json!({
            "object": "billing_subscription",
            "hard_limit_usd": 100_000_000.0,
            "soft_limit_usd": 100_000_000.0
        });
        assert_eq!(
            billing_limit_is_not_a_balance(&limit_only, None),
            Some(100_000_000.0),
            "只有 hard_limit_usd 时必须判为「不是余额」——上一版在这里算出了 99,998,820.48"
        );

        // ② 有更精确的 remaining → 不归本函数管（那个值本身就是余额）
        let with_remaining = serde_json::json!({
            "object": "billing_subscription",
            "remaining": 3.25,
            "hard_limit_usd": 10000.0
        });
        assert!(
            billing_limit_is_not_a_balance(&with_remaining, None).is_none(),
            "上游给了真实剩余时不得判成不可用"
        );
        assert_eq!(extract_balance(&with_remaining, None).remaining, Some(3.25));

        // ③ 用户显式指定了取值路径 → 一律尊重用户
        assert!(
            billing_limit_is_not_a_balance(&limit_only, Some("hard_limit_usd")).is_none(),
            "用户手填路径时不得自动改写其语义"
        );

        // ④ 文案可行动：上限原值 + 已用 + 出路，且带供探测循环识别的标识串
        let reason = billing_limit_reason(100_000_000.0, Some(1179.52), false);
        assert!(reason.contains("100000000"), "要把上限原值报出来：{reason}");
        assert!(reason.contains("1179.52"), "已用是准的，要说出来：{reason}");
        assert!(reason.contains("查询地址"), "要指出去哪里填：{reason}");
        assert!(
            reason.contains(BILLING_LIMIT_MARKER),
            "必须带标识串，否则探测循环挑不出这条有信息量的失败：{reason}"
        );

        // ⑤ `hard_limit_usd` 已从候选链里移除 —— 它绝不能再作为余额来源命中
        assert!(
            extract_balance(&limit_only, None).remaining.is_none(),
            "候选链里不该再有 hard_limit_usd"
        );

        // ⑥ usage 端点的单位换算：OpenAI 契约里 total_usage 是**美分**
        let usage = serde_json::json!({ "object": "list", "total_usage": 1234.5 });
        assert_eq!(
            parse_billing_usage_usd(&usage),
            Some(12.345),
            "total_usage 是美分，必须除以 100 才是 USD"
        );
        // data 包裹形态（部分二次开发站点）也要认
        let wrapped = serde_json::json!({ "data": { "total_usage": 500.0 } });
        assert_eq!(parse_billing_usage_usd(&wrapped), Some(5.0));
        // 没有该字段 → None（调用方据此保留上限值并如实标注，不编数字）
        assert_eq!(parse_billing_usage_usd(&serde_json::json!({ "object": "list" })), None);
    }

    /// SiliconFlow 同时返回 `balance`（赠送额度）、`chargeBalance`（充值额度）、
    /// `totalBalance`（二者之和）。候选链必须优先总额，不能被为 0 的赠送额度抢先。
    ///
    /// 回归（全业务对抗审查发现）：旧顺序 `data.balance` 在 `data.totalBalance` 前，
    /// `as_number("0.00")` 返回 `Some(0.0)`（0 是有效数字、find_map 立即停止），
    /// 所以「赠送额度用尽 + 仍有充值余额」会被误显成 0，违反本模块「绝不返回 0」。
    #[test]
    fn siliconflow_uses_total_balance_before_free_balance() {
        let body = serde_json::json!({
            "data": {
                "balance": "0.00",
                "chargeBalance": "5.00",
                "totalBalance": "5.00"
            }
        });
        let r = extract_balance(&body, None);
        assert!(r.ok, "SiliconFlow 标准响应应可解析：{:?}", r.error);
        assert_eq!(
            r.remaining,
            Some(5.0),
            "必须取 totalBalance=5，而非先命中 balance=0 误导用户账户已清零"
        );
    }

    /// OpenRouter：剩余额度是 `total_credits − total_usage` 的**减法**，不是单字段。
    ///
    /// 判据来源：cc-switch `src-tauri/src/services/balance.rs` 的 `openrouter` 函数
    /// （2026-08-17 从其源码核对：`remaining = total_credits - total_usage`，
    /// `is_valid = remaining > 0`）。
    ///
    /// **这条最容易被"优化"破坏**：若有人把减法删掉、只留候选链，泛化的
    /// `total_credits` 或 `data.limit_remaining` 会命中「总额」而不是「剩余」——
    /// 余额虚高，用户以为还有钱、实际已用完。故断言里同时钉住「不等于总额」。
    #[test]
    fn openrouter_remaining_is_credits_minus_usage() {
        // OpenRouter 实际返回形态（data 包裹）
        let body = serde_json::json!({
            "data": { "total_credits": 25.0, "total_usage": 18.75 }
        });
        let r = extract_balance(&body, None);
        assert!(r.ok, "OpenRouter 结构必须能解析：{:?}", r.error);
        assert_eq!(r.remaining, Some(6.25), "剩余 = 25 − 18.75；错了说明减法没生效");
        assert_ne!(
            r.remaining,
            Some(25.0),
            "绝不能把 total_credits 当剩余 —— 那样已用掉的 18.75 被无视，余额虚高"
        );

        // 根上直挂（无 data 包裹）也要认
        let flat = serde_json::json!({ "total_credits": 10.0, "total_usage": 3.0 });
        assert_eq!(extract_balance(&flat, None).remaining, Some(7.0));

        // 只有其中一个字段时不做减法，回落候选链（将来 OpenRouter 若直接给 remaining）
        let only_credits = serde_json::json!({ "total_credits": 10.0, "remaining": 4.0 });
        assert_eq!(
            extract_balance(&only_credits, None).remaining,
            Some(4.0),
            "凑不成减法时应回落候选链的 remaining"
        );

        // 用光后余额为 0：如实报 0，不能因为「像失败」就报错
        let spent = serde_json::json!({ "data": { "total_credits": 5.0, "total_usage": 5.0 } });
        let r = extract_balance(&spent, None);
        assert!(r.ok, "余额恰好用光是有效结果，不是错误");
        assert_eq!(r.remaining, Some(0.0));
    }

    /// Novita AI：`availableBalance` 的单位是 **0.0001 USD**，必须除以 10000。
    ///
    /// 判据来源：cc-switch 的 `novita` 函数（`availableBalance / 10000.0`，
    /// 其注释说明金额以 0.0001 USD 为单位）。
    ///
    /// 不缩放的后果是用户看到 `123456` 而真实余额是 `12.3456` —— 一个荒谬的大数，
    /// 用户只会以为程序坏了；而缩放错方向（乘 10000）则会把余额显示成 0，
    /// 让人误以为额度用光。两个方向都不可接受，故断言精确值。
    #[test]
    fn novita_available_balance_is_scaled_by_10000() {
        let body = serde_json::json!({ "availableBalance": 123_456 });
        let r = extract_balance(&body, None);
        assert!(r.ok, "Novita 结构必须能解析：{:?}", r.error);
        assert_eq!(
            r.remaining,
            Some(12.3456),
            "availableBalance 必须除以 10000（0.0001 USD 为单位）"
        );

        // 字符串形态同样要缩放（部分站点为避免精度问题用字符串传数字）
        let as_str = serde_json::json!({ "availableBalance": "98765" });
        assert_eq!(extract_balance(&as_str, None).remaining, Some(9.8765));

        // **不误伤**：别家的同名字段若在 data 下也按同规则缩放（已在 SCALED_FIELDS 里列出），
        // 但**其它字段一律不缩放** —— 这条钉住「缩放只对精确匹配的字段名生效」。
        let other = serde_json::json!({ "balance": 123_456 });
        assert_eq!(
            extract_balance(&other, None).remaining,
            Some(123_456.0),
            "普通 balance 字段不得被缩放"
        );
    }

    /// 月之暗面 Kimi 的真实响应必须能解析，且**不得**被 Novita 的 /10000 规则误伤。
    ///
    /// 这条钉的是一个「支持了一半」的缺陷：`VENDOR_ENDPOINTS` 里有
    /// `api.moonshot.cn → /v1/users/me/balance`（本项目补的，cc-switch 没有），
    /// 但候选链里只有 camelCase 的 `availableBalance`（Novita），没有 Kimi 的
    /// snake_case `available_balance`。于是 Kimi 用户零配置拿到正确 URL + 200 响应，
    /// 却收到「上游返回里找不到余额字段」—— 报错把人指向「改取值路径」，而地址是对的。
    ///
    /// 两个断言方向都必须钉住：
    /// 1. 取得到值（否则等于没支持）；
    /// 2. 值**没有**被除以 10000（把 49.58 显示成 0.0049 会被当成额度耗尽）。
    ///    两个字段名只差一个下划线、量纲完全不同，这是最容易被后人「统一处理」掉的地方。
    #[test]
    fn moonshot_snake_case_available_balance_is_read_and_not_scaled() {
        // 实测形态（platform.kimi.ai 的 Check Balance：available / voucher / cash 三项）
        let body = serde_json::json!({
            "code": 0,
            "data": { "available_balance": 49.58, "voucher_balance": 46.58, "cash_balance": 3.0 },
            "scode": "0x0",
            "status": true
        });
        let r = extract_balance(&body, None);
        assert!(r.ok, "Kimi 结构必须能解析：{:?}", r.error);
        assert_eq!(
            r.remaining,
            Some(49.58),
            "取 data.available_balance 原值；被 /10000 缩放成 0.0049 会被误读为额度耗尽"
        );

        // 无 data 包裹的形态（同一字段名放在根上）也要认，候选链本就是为形态差异而存在
        let flat = serde_json::json!({ "available_balance": "12.34" });
        assert_eq!(extract_balance(&flat, None).remaining, Some(12.34));

        // 与 Novita 的 camelCase 分道扬镳：同一个测试里对照，防后人合并两条
        let novita = serde_json::json!({ "availableBalance": 123_456 });
        assert_eq!(
            extract_balance(&novita, None).remaining,
            Some(12.3456),
            "camelCase 版仍须缩放 —— 两个字段不是一回事"
        );
    }

    /// 上游明确说「账号无效」时，报的是**那个原因**而不是「找不到字段」。
    ///
    /// 两者的处理方式完全不同：前者要去充值/换号，后者要去改配置。
    /// 混成一句「找不到余额字段」会把用户引向错误的方向。
    #[test]
    fn invalid_account_reports_reason_not_missing_field() {
        let body = serde_json::json!({ "is_active": false, "message": "账号已停用" });
        let r = extract_balance(&body, None);
        assert!(!r.ok);
        assert_eq!(r.is_valid, Some(false));
        assert_eq!(r.invalid_message.as_deref(), Some("账号已停用"));
        assert!(
            !r.error.as_deref().unwrap().contains("找不到余额字段"),
            "账号无效时不该报「找不到字段」，实际: {:?}",
            r.error
        );
    }

    /// cc-switch 契约里的 `total` / `used` / `planName` 也要能取到。
    #[test]
    fn extracts_total_used_and_plan_name() {
        let body = serde_json::json!({
            "remaining": 30.0, "total": 100.0, "used": 70.0,
            "planName": "Pro", "unit": "USD"
        });
        let r = extract_balance(&body, None);
        assert_eq!(r.total, Some(100.0));
        assert_eq!(r.used, Some(70.0));
        assert_eq!(r.plan_name.as_deref(), Some("Pro"));

        // NewAPI 风格：套餐名在 data.group
        let n = serde_json::json!({ "data": { "quota": 5.0, "group": "vip" } });
        assert_eq!(extract_balance(&n, None).plan_name.as_deref(), Some("vip"));
    }

    /// 取不到余额时**绝不能**返回 0：那会让用户以为额度用光了。
    #[test]
    fn missing_field_reports_error_not_zero() {
        let body = serde_json::json!({ "some_other_field": 123 });
        let r = extract_balance(&body, None);
        assert!(!r.ok);
        assert_eq!(r.remaining, None, "取不到就必须是 None，绝不是 Some(0.0)");
        assert!(r.error.is_some(), "必须带上失败原因");
        assert!(
            r.error.as_deref().unwrap().contains("取值路径"),
            "错误信息应提示用户可手填路径，实际: {:?}",
            r.error
        );
    }

    /// 用户显式指定路径时只认它，且填错要如实报错（不要静默回退到自动探测）。
    #[test]
    fn custom_path_is_authoritative() {
        let body = serde_json::json!({
            "remaining": 999,                    // 自动探测会命中这个
            "wallet": { "left": 42.5 }
        });

        // 指定路径优先
        assert_eq!(extract_balance(&body, Some("wallet.left")).remaining, Some(42.5));

        // 路径填错：报错而不是回退到 remaining=999
        let r = extract_balance(&body, Some("wallet.nonexistent"));
        assert!(!r.ok);
        assert_eq!(
            r.remaining, None,
            "指定路径取不到时不得静默回退到自动探测，否则用户永远发现不了路径填错了"
        );
        assert!(r.error.as_deref().unwrap().contains("wallet.nonexistent"));

        // 空白路径视为「未指定」，走自动探测
        assert_eq!(extract_balance(&body, Some("  ")).remaining, Some(999.0));
    }

    /// 测试用的简写：只关心 baseUrl / apiKey 两个占位符时不必写满五个参数。
    fn expand(tpl: &str, base: &str, key: &str) -> String {
        expand_placeholders(tpl, base, key, "", "")
    }

    #[test]
    fn placeholders_expand_and_avoid_double_slash() {
        assert_eq!(
            expand("{{baseUrl}}/user/balance", "https://api.foo.com", "sk-1"),
            "https://api.foo.com/user/balance"
        );
        // base_url 带尾斜杠时不得拼出 `//user/balance`（部分网关对此返回 404）
        assert_eq!(
            expand("{{baseUrl}}/user/balance", "https://api.foo.com/", "sk-1"),
            "https://api.foo.com/user/balance"
        );
        // apiKey 占位符（少数站点要求密钥在 query 里）
        assert_eq!(
            expand("{{baseUrl}}/q?key={{apiKey}}", "https://a.com", "sk-2"),
            "https://a.com/q?key=sk-2"
        );
    }

    /// `{{origin}}` 必须剥掉 baseUrl 的路径部分。
    ///
    /// 这条为一个实测缺陷立的：DeepSeek 的 baseUrl 是
    /// `https://api.deepseek.com/anthropic`（带路径后缀），而余额端点在**域名根**下。
    /// 用 `{{baseUrl}}/user/balance` 拼出 `.../anthropic/user/balance` → 实测 404；
    /// 换成 `{{origin}}/user/balance` 才是 `https://api.deepseek.com/user/balance` → 200。
    #[test]
    fn origin_placeholder_strips_path_suffix() {
        assert_eq!(
            expand("{{origin}}/user/balance", "https://api.deepseek.com/anthropic", "sk"),
            "https://api.deepseek.com/user/balance"
        );
        // 同一个 baseUrl 下两个占位符的差别 —— 这正是 404 与 200 的分界
        assert_eq!(
            expand("{{baseUrl}}/user/balance", "https://api.deepseek.com/anthropic", "sk"),
            "https://api.deepseek.com/anthropic/user/balance"
        );
        // 无路径后缀时两者等价
        assert_eq!(
            expand("{{origin}}/x", "https://a.com", "sk"),
            expand("{{baseUrl}}/x", "https://a.com", "sk")
        );
        // 多级路径也要剥干净
        assert_eq!(
            expand("{{origin}}/b", "https://a.com/v1/foo/bar", "sk"),
            "https://a.com/b"
        );
    }

    /// NewAPI 模板的两个占位符（对齐 cc-switch 文档 §2.5）。
    #[test]
    fn newapi_placeholders_expand() {
        assert_eq!(
            expand_placeholders(
                "{{baseUrl}}/api/user/self?id={{userId}}",
                "https://panel.foo.com",
                "sk-unused",
                "tok-abc",
                "42",
            ),
            "https://panel.foo.com/api/user/self?id=42"
        );
        assert_eq!(
            expand_placeholders("{{accessToken}}", "https://a.com", "sk", "tok-abc", "1"),
            "tok-abc"
        );
    }

    #[tokio::test]
    async fn disabled_or_misconfigured_fails_fast_without_network() {
        let key = ProviderKey {
            id: "k1".into(),
            category_id: crate::model::CategoryType::ClaudeCli,
            name: "t".into(),
            vendor: "v".into(),
            base_url: "https://api.foo.com".into(),
            protocol: crate::model::Protocol::Anthropic,
            ..Default::default()
        };

        // 未启用
        let off = BalanceQuery { enabled: false, ..Default::default() };
        assert!(!query_balance(&key, &off, "sk").await.ok);

        // 启用但没填地址：**不再报「未配置查询地址」** —— 自动识别会按 base_url 给一个
        // 兜底端点（这条断言随自动识别功能一起改，原先的行为是直接失败）。
        // 这里 base_url 是 api.foo.com（认不出的域名），故走 FALLBACK_ENDPOINT，
        // 会真的去打网络 —— 本测试只验「不因缺地址而立即失败」，不验网络结果。
        let no_url = BalanceQuery { enabled: true, url: "  ".into(), ..Default::default() };
        let r = query_balance(&key, &no_url, "sk").await;
        assert!(
            !r.error.as_deref().unwrap_or("").contains("未配置查询地址"),
            "自动识别接上后不该再因缺地址而拒绝，实际: {:?}",
            r.error
        );

        // 展开后不是合法 URL（用户把 baseUrl 占位符打错成 baseurl）。
        // 注意 template 必须非 auto，否则自动识别会忽略这个坏 url。
        let bad = BalanceQuery {
            enabled: true,
            template: "custom".into(),
            url: "{{baseurl}}/v1/usage".into(),
            ..Default::default()
        };
        let r = query_balance(&key, &bad, "sk").await;
        assert!(!r.ok);
        assert!(
            r.error.as_deref().unwrap().contains("不是合法"),
            "实际: {:?}",
            r.error
        );

        // 不支持的方法 / 认证方式也要早报错
        let bad_method = BalanceQuery {
            enabled: true,
            template: "custom".into(),
            url: "{{baseUrl}}/x".into(),
            method: "DELETE".into(),
            ..Default::default()
        };
        assert!(query_balance(&key, &bad_method, "sk").await.error.is_some());
    }

    /// 按 `base_url` 自动识别厂商端点：cc-switch 的 5 家 + 本项目补的几家都要命中。
    ///
    /// 这是「余额查询一直不行」的正面修复 —— 用户不再需要判断自己的站属于哪一类。
    /// 判据全部对齐 cc-switch 源码里的实际端点（2026-08-17 逐函数核对），
    /// 改动其中任一条前请先核对上游文档，别凭印象改。
    #[test]
    fn detect_balance_endpoint_covers_known_vendors() {
        // cc-switch 的 5 家
        assert_eq!(
            detect_balance_endpoint("https://api.deepseek.com/anthropic").0,
            "{{origin}}/user/balance",
            "DeepSeek 的余额在域名根下，必须用 origin 剥掉 /anthropic 后缀"
        );
        assert_eq!(
            detect_balance_endpoint("https://api.siliconflow.cn/v1").0,
            "{{origin}}/v1/user/info"
        );
        assert_eq!(
            detect_balance_endpoint("https://openrouter.ai/api/v1").0,
            "{{origin}}/api/v1/credits"
        );
        assert_eq!(
            detect_balance_endpoint("https://api.novita.ai/v3/openai").0,
            "{{origin}}/v3/user/balance"
        );
        // StepFun 的 .ai 域名要改打 .com（对齐 cc-switch：匹配两个域名但恒发 .com）
        assert_eq!(
            detect_balance_endpoint("https://api.stepfun.ai/v1").0,
            "https://api.stepfun.com/v1/accounts",
            "cc-switch 的 StepFun 分支匹配 .ai 但请求发 .com"
        );

        // 官方 Anthropic 的认证头不是 bearer
        let (url, auth) = detect_balance_endpoint("https://api.anthropic.com");
        assert_eq!(url, "{{origin}}/v1/organizations/me");
        assert_eq!(auth, "x-api-key", "官方 Anthropic 用 x-api-key 而非 Bearer");

        // StepFun 的 .com 直连（区别于上面 .ai→.com 的改写）：用 origin
        assert_eq!(
            detect_balance_endpoint("https://api.stepfun.com/v1").0,
            "{{origin}}/v1/accounts",
            ".com 域名直接用 origin，不改写"
        );
        // SiliconFlow 的 .com 域名（与 .cn 同端点）
        assert_eq!(
            detect_balance_endpoint("https://api.siliconflow.com/v1").0,
            "{{origin}}/v1/user/info"
        );
        // 本项目实测补的三家（cc-switch 没有）——不加断言就等于「加了映射却没锁住」，
        // 日后有人误删或改错端点不会被测试发现。
        assert_eq!(
            detect_balance_endpoint("https://open.bigmodel.cn/api/paas/v4").0,
            "{{origin}}/api/paas/v4/account/balance",
            "智谱 GLM 开放平台额度接口"
        );
        assert_eq!(
            detect_balance_endpoint("https://api.moonshot.cn/v1").0,
            "{{origin}}/v1/users/me/balance",
            "月之暗面 Kimi（.cn）"
        );
        assert_eq!(
            detect_balance_endpoint("https://api.moonshot.ai/v1").0,
            "{{origin}}/v1/users/me/balance",
            "月之暗面 Kimi（.ai）"
        );

        // 认不出的域名 → 兜底链的**第一条**（`/v1/usage`，cc-switch 给中转站实配的那条）。
        // 注意 `detect_balance_endpoint` 只给一个代表值；查询路径走的是
        // `resolve_endpoint_candidates`，会把整条链按序试完
        // （见 unknown_vendor_probes_usage_endpoint_first）。
        let (url, auth) = detect_balance_endpoint("https://www.some-relay-station.net");
        assert_eq!(url, "{{origin}}/v1/usage");
        assert_eq!(auth, "bearer");

        // 大小写不敏感：用户可能把域名写成大写
        assert_eq!(
            detect_balance_endpoint("HTTPS://API.DeepSeek.COM/v1").0,
            "{{origin}}/user/balance"
        );
    }

    /// **用户填的地址必须优先于自动识别** —— 这条是自动识别的安全边界。
    ///
    /// 若自动识别覆盖了用户填的地址，就成了「我改的不生效」这类静默失效（本项目最忌讳
    /// 的形态）：用户改了地址、报错却还是老样子，他会以为改动没保存。反过来自动识别
    /// 猜错时，用户手填就是唯一出路，必须留着。
    ///
    /// 判据设计：给一个**能被自动识别命中**的 base_url（deepseek），同时手填一个
    /// 刻意打错占位符的 url。
    /// - 用户地址被尊重 → `{{bad_placeholder}}` 展不开 → 报「不是合法 URL」
    /// - 被自动识别覆盖 → 变成合法的 deepseek 端点 → 不会报这个错
    ///
    /// 故障注入验证：把 `url_template` 的三元改成无条件用 `detected.0`，此断言立刻变红。
    #[tokio::test]
    async fn user_filled_url_beats_auto_detection() {
        let key = ProviderKey {
            id: "k".into(),
            category_id: crate::model::CategoryType::ClaudeCli,
            name: "t".into(),
            vendor: "v".into(),
            // 这个域名会被自动识别命中（→ {{origin}}/user/balance）
            base_url: "https://api.deepseek.com/anthropic".into(),
            protocol: crate::model::Protocol::Anthropic,
            ..Default::default()
        };

        let user_choice = BalanceQuery {
            enabled: true,
            template: "custom".into(),
            url: "{{bad_placeholder}}/my/own/path".into(),
            ..Default::default()
        };
        let r = query_balance(&key, &user_choice, "sk").await;
        assert!(
            r.error.as_deref().unwrap_or("").contains("不是合法"),
            "用户填的地址必须被使用（哪怕它是错的）；被自动识别覆盖了就是静默失效。实际: {:?}",
            r.error
        );

        // 反面：url 留空时才该用自动识别。用「地址合法性」区分两条分支 ——
        // 自动识别给的是合法 URL，故不会报「不是合法」（会走到网络请求）。
        let auto = BalanceQuery {
            enabled: true,
            url: String::new(),
            ..Default::default()
        };
        let r = query_balance(&key, &auto, "sk").await;
        assert!(
            !r.error.as_deref().unwrap_or("").contains("不是合法"),
            "url 留空时该用自动识别给出的合法端点，实际: {:?}",
            r.error
        );
    }

    /// P3-3 修复配套：`resolve_endpoint_candidates` 的四个分支（url 填/空 × auth 填/空）逐一钉住。
    ///
    /// 抽出纯函数就是为了能不打网络验这两条独立判空。此前只有 url 分支被 `query_balance`
    /// 的「不是合法 URL」间接覆盖，auth 分支完全没测——用户手选 x-api-key 却被自动识别
    /// 覆盖成 bearer 这类静默失效不会被发现。
    ///
    /// 本用例全部用**能被域名识别**的 base（deepseek），故候选恒为 1 条 ——
    /// 「认不出时按序探测多条」是另一条用例
    /// （`unknown_vendor_probes_usage_endpoint_first`）的事。
    #[test]
    fn resolve_endpoint_fills_only_empty_fields() {
        // base_url 命中 deepseek（自动识别 → url={{origin}}/user/balance, auth=bearer）
        let base = "https://api.deepseek.com/anthropic";
        let one = |cfg: &BalanceQuery| -> (String, String) {
            let c = resolve_endpoint_candidates(cfg, base);
            assert_eq!(c.len(), 1, "域名认得出时不该探测多条");
            (c[0].0.to_string(), c[0].1.to_string())
        };

        // ① url 与 auth 都留空 → 全用自动识别
        let cfg = BalanceQuery { enabled: true, ..Default::default() };
        let (u, a) = one(&cfg);
        assert_eq!(u, "{{origin}}/user/balance", "url 留空该用自动识别");
        assert_eq!(a, "bearer", "auth 留空该用自动识别");

        // ② url 填了、auth 留空 → url 用用户的、auth 仍自动识别（两者独立判空）
        let cfg = BalanceQuery {
            enabled: true,
            url: "https://panel.example.com/my/balance".into(),
            ..Default::default()
        };
        let (u, a) = one(&cfg);
        assert_eq!(u, "https://panel.example.com/my/balance", "用户填的 url 优先");
        assert_eq!(a, "bearer", "auth 没填仍走自动识别");

        // ③ auth 填了、url 留空 → auth 用用户的、url 仍自动识别
        //    这正是 P3-3 未覆盖的分支：用户手选 x-api-key 不该被自动识别的 bearer 覆盖
        let cfg = BalanceQuery {
            enabled: true,
            auth: "x-api-key".into(),
            ..Default::default()
        };
        let (u, a) = one(&cfg);
        assert_eq!(u, "{{origin}}/user/balance", "url 没填走自动识别");
        assert_eq!(a, "x-api-key", "用户手选的 auth 必须优先，不被自动识别覆盖");

        // ④ 都填了 → 全用用户的
        let cfg = BalanceQuery {
            enabled: true,
            url: "https://x.com/b".into(),
            auth: "none".into(),
            ..Default::default()
        };
        let (u, a) = one(&cfg);
        assert_eq!(u, "https://x.com/b");
        assert_eq!(a, "none");
    }

    /// 认不出域名的中转站必须按序探测，且 **`/v1/usage` 排第一**。
    ///
    /// 这是「su2api 恒显示 10000 USD」的直接修复。原先兜底只有一条
    /// `/v1/dashboard/billing/subscription`，而 NewAPI 系在那个端点返回的是
    /// `hard_limit_usd`（配额**上限**）—— 一个永不变化的 10000。
    ///
    /// **判据来自用户本机 cc-switch 库**（读 `providers.meta`，非推测）：Sub2API
    /// （sub.100xlabs.space）与「林夕」公益站（k40.shengqainbang.cn）配的
    /// `usage_script` 端点都是 `{{baseUrl}}/v1/usage`。
    ///
    /// 四个方向一起钉，少一条这个修复就会被后人「简化」掉：
    /// 1. 认不出的站 → 多条候选，且第一条是 `/v1/usage`；
    /// 2. `hard_limit_usd` 那条必须**排最后**（它能出数，但那是上限，会掩盖前面的正确答案）；
    /// 3. 用户填了地址 → **只用他填的那条，绝不探测**（否则「我改的不生效」）；
    /// 4. 域名认得出 → 也只一条（对 11 个已取证厂商探测纯属多打请求）。
    #[test]
    fn unknown_vendor_probes_usage_endpoint_first() {
        let cfg = BalanceQuery { enabled: true, ..Default::default() };

        // ① 认不出的中转站：多条候选，第一条是 /v1/usage
        let c = resolve_endpoint_candidates(&cfg, "https://sub.100xlabs.space");
        assert!(c.len() > 1, "认不出域名时必须按序探测多条，实际 {} 条", c.len());
        assert_eq!(
            c[0].0, "{{origin}}/v1/usage",
            "第一条必须是 /v1/usage —— cc-switch 给 Sub2API / 林夕公益站实配的就是它"
        );
        // ② hard_limit_usd 那条排最后
        assert_eq!(
            c.last().unwrap().0,
            "{{origin}}/v1/dashboard/billing/subscription",
            "billing/subscription 返回的是配额上限而非余额，必须排最后 —— \
             排前面就会用一个恒为 10000 的数字盖掉后面正确的答案"
        );

        // ③ 用户填了地址 → 只用他填的，不探测
        let filled = BalanceQuery {
            enabled: true,
            url: "https://my.panel/quota".into(),
            ..Default::default()
        };
        let c = resolve_endpoint_candidates(&filled, "https://sub.100xlabs.space");
        assert_eq!(c.len(), 1, "用户填了地址就不该再探测别的（否则「我改的不生效」）");
        assert_eq!(c[0].0, "https://my.panel/quota");

        // ④ 域名认得出 → 也只一条
        let c = resolve_endpoint_candidates(&cfg, "https://api.deepseek.com");
        assert_eq!(c.len(), 1, "已取证的厂商不该被探测");
        assert_eq!(c[0].0, "{{origin}}/user/balance");
    }

    /// 余额查询**必须带客户端身份头（UA）**。
    ///
    /// 这条测试为一个实测缺陷立的：`shared_client()` 不设默认 UA（reqwest 默认不发），
    /// 而部分中转渠道靠 UA 做客户端准入 —— 缺了就判 `detected: unknown` 直接 403。
    /// 现象是「cc-switch 能查出余额、SynaRoute 查不出」，两边配置却一模一样，
    /// 极难联想到是请求头的问题。
    ///
    /// 用真实的 `reqwest::RequestBuilder` 走一遍 `apply_client_identity`，
    /// 再从构建出的 `Request` 上读回头部 —— 直接验「头确实被加上了」，
    /// 而不是验「代码里写了那一行」。
    #[test]
    fn balance_request_carries_client_identity_headers() {
        use crate::model::Protocol;

        let client = crate::upstream::shared_client();
        for (protocol, expect_ua_contains) in [
            (Protocol::Anthropic, "claude"),
            (Protocol::OpenaiChat, "codex"),
        ] {
            let req = client.get("https://example.com/v1/usage");
            let req = crate::upstream::apply_client_identity(req, protocol);
            let built = req.build().expect("构建请求应成功");
            let ua = built
                .headers()
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                ua.contains(expect_ua_contains),
                "{protocol:?} 的 UA 应含 `{expect_ua_contains}`，实际: {ua:?}"
            );
        }
    }
}
