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
    "balance",
    "data.balance",
    "data.remaining",
    "data.quota.remaining",
    // ⚠️ `data.quota`：NewAPI/OneAPI 架构里这是**内部计费单位**（默认 500000 = 1 USD，且
    // QuotaPerUnit 由站长可配），不是 USD。这里**刻意不缩放**：硬编码 500000 对改过配额比率的
    // 实例会错，且若别家用 `data.quota` 存的本就是 USD，除以 50 万会变约 0 → 反而违反「绝不返回
    // 0」。NewAPI 中转站的**正确**取值路径是 relay 模板的 `hard_limit_usd`（已是 USD，见下）；
    // 此项仅作没有更精确字段时的兜底，命中它时数值可能偏大，属已知局限（详见余额文档）。
    "data.quota",
    "credit",
    "data.credit",
    // OpenRouter：`{"data":{"limit_remaining":...}}`
    "data.limit_remaining",
    "total_available",
    "data.total_available",
    // NewAPI / OpenAI 兼容计费层：`GET /v1/dashboard/billing/subscription` 返回
    // `{"object":"billing_subscription","hard_limit_usd":10.5,"soft_limit_usd":…}`。
    //
    // **实测来源**（2026-08-16，sotamodel.net）：该站是 NewAPI 架构，`/user/balance`
    // 返回网页、`/api/user/self` 要面板 access token，只有这条认转发用的 API Key
    // （无效 Key 时返 `{"error":{…,"type":"new_api_error"}}`）。中转站普遍如此，
    // 而此前候选链里没有 hard_limit_usd —— 用户即便填对了地址也取不到值。
    //
    // 排在末尾：`hard_limit_usd` 是「额度上限」而非严格意义的「剩余」，
    // 前面任一更精确的字段命中时都不该被它抢先。
    "hard_limit_usd",
    "data.hard_limit_usd",
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

/// 认不出具体厂商时的兜底端点链（按命中概率排序）。
///
/// 为什么值得有：中转站占本项目用户的绝大多数，而它们几乎都是 NewAPI / OneAPI 系
/// 的二次开发 —— 端点高度收敛到这几条。逐条试比让用户在 5 个模板里盲猜靠谱得多。
///
/// **刻意不在这里做多端点轮询**：那会对上游发 N 次请求（部分站点按请求计费/限流）。
/// 这里只提供「第一条」作为自动模板的默认值，试不中时错误信息会把其余候选列给用户
/// （见 `looks_like_html` 分支的提示文案）。
const FALLBACK_ENDPOINT: (&str, &str) = ("{{origin}}/v1/dashboard/billing/subscription", "bearer");

/// 按 `base_url` 猜该站点的余额端点。返回 `(url 模板, 认证方式)`。
///
/// 命中 [`VENDOR_ENDPOINTS`] 里的域名就用那家的；认不出则用 [`FALLBACK_ENDPOINT`]
/// （NewAPI/OneAPI 系的通用计费端点，中转站命中率最高的一条）。
///
/// 大小写不敏感：用户可能把域名写成 `API.DeepSeek.com`。
pub fn detect_balance_endpoint(base_url: &str) -> (&'static str, &'static str) {
    let lower = base_url.to_ascii_lowercase();
    for (domain, url, auth) in VENDOR_ENDPOINTS {
        if lower.contains(domain) {
            return (url, auth);
        }
    }
    FALLBACK_ENDPOINT
}

/// 解析本次查询实际用的 `(url 模板, 认证方式)`：**用户填了什么就用什么，没填的才自动补**。
///
/// 规则就一条（url 与 auth 各自独立判空）：
///   - 非空 → 用用户的（哪怕 url 是错的：那样他才能从报错里看出自己填错了；被自动识别
///     悄悄改成能用的地址反而是「我改的不生效」这类静默失效）
///   - 为空 → 用 `detect_balance_endpoint` 按域名猜的
///
/// 刻意**不看 `template` 字段**：它只是界面上「用户点了哪个按钮」的回显，真正决定行为的
/// 是 url/auth 有没有值。早先按 template 判 auto 模式的写法与这条判空规则重复、恒同向，
/// 是死代码（故障注入实测：把 auto_mode 恒置 true 行为不变）。
///
/// 抽成纯函数是为了让「用户填优先 / 留空回退」两条分支能脱离网络单测（`query_balance`
/// 要真打上游才走到 auth，无法在单测里观察 auth 解析结果）。
fn resolve_endpoint<'a>(cfg: &'a BalanceQuery, base: &str) -> (&'a str, &'a str) {
    let detected = detect_balance_endpoint(base);
    let url = if cfg.url.trim().is_empty() { detected.0 } else { cfg.url.trim() };
    let auth = if cfg.auth.trim().is_empty() { detected.1 } else { cfg.auth.trim() };
    (url, auth)
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
    }
}

/// 对单个 Key 执行一次余额查询。
///
/// `secret` 由调用方从密钥库取出（本模块不碰密钥库，保持可测）。
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

    // 自动识别：按 base_url 域名猜端点，**只填补用户没填的那一项**。见 resolve_endpoint。
    let (url_template, auth_scheme) = resolve_endpoint(cfg, base);

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

    let client = crate::upstream::shared_client();
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
        Ok(v) => extract_balance(&v, cfg.remaining_path.as_deref()),
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
    /// **为什么这条必须有**（2026-08-16 实测 sotamodel.net 得出）：中转站普遍是 NewAPI 架构，
    /// 而它三个候选端点的行为各不相同：
    ///   - `/user/balance` → 200 但返回**网页**（generic 模板打到这里，用户一直失败）
    ///   - `/api/user/self` → 要面板 access token，不是转发用的 API Key
    ///   - `/v1/dashboard/billing/subscription` → **只有这条认 API Key**
    ///
    /// 而这条返回里的余额字段是 `hard_limit_usd`，此前**不在候选链里** —— 即用户就算
    /// 填对了地址也取不到值，只会看到「上游返回里找不到余额字段」。
    /// 删掉候选链末尾那两条 `hard_limit_usd` 后本测试必红。
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
            r.ok,
            "NewAPI billing_subscription 必须能解析（hard_limit_usd 在候选链里吗？）：{:?}",
            r.error
        );
        assert_eq!(
            r.remaining,
            Some(10.5),
            "余额取 hard_limit_usd；取不到说明候选链缺这个字段"
        );

        // 更精确的字段存在时不得被 hard_limit_usd 抢先（它排在候选链末尾正是为此）
        let with_remaining = serde_json::json!({
            "object": "billing_subscription",
            "remaining": 3.25,
            "hard_limit_usd": 10.5
        });
        assert_eq!(
            extract_balance(&with_remaining, None).remaining,
            Some(3.25),
            "remaining 比 hard_limit_usd 精确，必须优先"
        );
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

        // 认不出的域名 → 兜底到 NewAPI/OneAPI 系的通用计费端点（中转站命中率最高）
        let (url, auth) = detect_balance_endpoint("https://www.some-relay-station.net");
        assert_eq!(url, "{{origin}}/v1/dashboard/billing/subscription");
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

    /// P3-3 修复配套：`resolve_endpoint` 的四个分支（url 填/空 × auth 填/空）逐一钉住。
    ///
    /// 抽出纯函数就是为了能不打网络验这两条独立判空。此前只有 url 分支被 `query_balance`
    /// 的「不是合法 URL」间接覆盖，auth 分支完全没测——用户手选 x-api-key 却被自动识别
    /// 覆盖成 bearer 这类静默失效不会被发现。
    #[test]
    fn resolve_endpoint_fills_only_empty_fields() {
        // base_url 命中 deepseek（自动识别 → url={{origin}}/user/balance, auth=bearer）
        let base = "https://api.deepseek.com/anthropic";

        // ① url 与 auth 都留空 → 全用自动识别
        let cfg = BalanceQuery { enabled: true, ..Default::default() };
        let (u, a) = resolve_endpoint(&cfg, base);
        assert_eq!(u, "{{origin}}/user/balance", "url 留空该用自动识别");
        assert_eq!(a, "bearer", "auth 留空该用自动识别");

        // ② url 填了、auth 留空 → url 用用户的、auth 仍自动识别（两者独立判空）
        let cfg = BalanceQuery {
            enabled: true,
            url: "https://panel.example.com/my/balance".into(),
            ..Default::default()
        };
        let (u, a) = resolve_endpoint(&cfg, base);
        assert_eq!(u, "https://panel.example.com/my/balance", "用户填的 url 优先");
        assert_eq!(a, "bearer", "auth 没填仍走自动识别");

        // ③ auth 填了、url 留空 → auth 用用户的、url 仍自动识别
        //    这正是 P3-3 未覆盖的分支：用户手选 x-api-key 不该被自动识别的 bearer 覆盖
        let cfg = BalanceQuery {
            enabled: true,
            auth: "x-api-key".into(),
            ..Default::default()
        };
        let (u, a) = resolve_endpoint(&cfg, base);
        assert_eq!(u, "{{origin}}/user/balance", "url 没填走自动识别");
        assert_eq!(a, "x-api-key", "用户手选的 auth 必须优先，不被自动识别覆盖");

        // ④ 都填了 → 全用用户的
        let cfg = BalanceQuery {
            enabled: true,
            url: "https://x.com/b".into(),
            auth: "none".into(),
            ..Default::default()
        };
        let (u, a) = resolve_endpoint(&cfg, base);
        assert_eq!(u, "https://x.com/b");
        assert_eq!(a, "none");
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
