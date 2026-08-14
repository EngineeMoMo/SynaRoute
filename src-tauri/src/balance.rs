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
    "balance",
    "data.balance",
    "data.remaining",
    "data.quota.remaining",
    "data.quota",
    "credit",
    "data.credit",
    // SiliconFlow 系：`{"data":{"totalBalance":"..."}}`
    "data.totalBalance",
    "totalBalance",
    // OpenRouter：`{"data":{"limit_remaining":...}}`
    "data.limit_remaining",
    "total_available",
    "data.total_available",
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
        None => find_number(body, REMAINING_CANDIDATES),
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
    if cfg.url.trim().is_empty() {
        return BalanceResult::failed("未配置查询地址");
    }

    let base = cfg
        .base_url_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&key.base_url);

    // NewAPI 类面板用 accessToken + userId 而非 API Key，故一并展开。
    // 两者留空时展开成空串，模板里没用到就无影响。
    let access_token = cfg.access_token.as_deref().unwrap_or("");
    let user_id = cfg.user_id.as_deref().unwrap_or("");
    let url = expand_placeholders(&cfg.url, base, secret, access_token, user_id);
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
    req = match cfg.auth.trim().to_ascii_lowercase().as_str() {
        "" | "bearer" => req.bearer_auth(secret),
        "x-api-key" => req.header("x-api-key", secret),
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

        // 启用但没填地址
        let no_url = BalanceQuery { enabled: true, url: "  ".into(), ..Default::default() };
        let r = query_balance(&key, &no_url, "sk").await;
        assert!(!r.ok);
        assert!(r.error.as_deref().unwrap().contains("未配置查询地址"));

        // 展开后不是合法 URL（用户把 baseUrl 占位符打错成 baseurl）
        let bad = BalanceQuery {
            enabled: true,
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
            method: "DELETE".into(),
            ..Default::default()
        };
        assert!(query_balance(&key, &bad_method, "sk").await.error.is_some());
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
