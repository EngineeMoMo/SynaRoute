//! `ProviderKey.headers_json` 的接线（docs/14 §21.1 B1）。
//!
//! 挂在 [`crate::proxy`] 下（`#[path]`）—— `proxy.rs` 棘轮余量为 0，
//! 而目录化是 docs/15 P2 刻意未做的大 diff。同 `lan_guard.rs` / `log_rotate.rs` 的挂法。
//!
//! # 此前的状态
//!
//! 字段在 `model.rs:405` 定义、能落盘、诊断导出会脱敏它，但**转发零读取、前端零 UI**。
//! 也就是一个只存在于数据结构里的字段。真实需求是有的：OpenRouter 要
//! `HTTP-Referer`/`X-Title`，部分中转站要自有标识头。
//!
//! # 两道防线，方向不同，缺一不可
//!
//! 1. **保存时拒绝**（[`reject_reserved`]）—— 用户填了保留字段就报错并说明。
//!    **不能静默忽略**：那会让人以为生效了，然后去查中转站/网络，
//!    而真正的原因是我们悄悄丢了他的输入（本仓「静默失效」那一类缺陷）。
//! 2. **转发时过滤**（[`headers_for`]）—— 这个字段在有 UI 之前就存在了，
//!    用户可能手改过 `config.json`，老配置里也可能已有值。
//!    放一个 `authorization` 过去会**覆盖我们换上的真实 Key** → 上游 401，
//!    而日志只显示「鉴权失败」，没人会想到是自定义头干的。
//!
//! # 保留字段清单就是 `is_reserved`，**不许再抄第二份**
//!
//! 这个判据原先叫 `proxy.rs` 的 `is_stripped_header`（只用于「哪些下游头不透传」），
//! 现在移到这里并改名，因为它同时是**自定义头的黑名单**。
//! 两个用途必须共用一份清单 —— 否则加一个新的代理自有头时只改一处，
//! 另一处就成了洞，且失效是静默的。

use crate::model::ProviderKey;

/// 代理自有、**不许被下游透传也不许被用户自定义头覆盖**的请求头。
///
/// 各条的理由：鉴权（必须用本 Key 的密钥）、`anthropic-version`（按 Key 协议定）、
/// `host`/`content-length`/`content-type`（reqwest 重算）、
/// `accept-encoding`（刻意设 `identity`，见 `apply_upstream_headers` 那段注释：
/// 转发路径字节透明，上游一压缩下游就是乱码）、
/// 以及 RFC 7230 的逐跳头（按定义不能原样转发）。
pub(crate) fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "x-api-key"
            | "anthropic-version"
            | "host"
            | "content-length"
            | "content-type"
            | "accept-encoding"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// 头值里不允许出现的字符：CR/LF 会造成**响应头注入**（把一行拆成多行，
/// 从而伪造出额外的头）。reqwest 自己也会拒，但那时报的是一句
/// 看不出成因的 `InvalidHeaderValue`，而这里能在保存时就说清。
fn has_control_char(v: &str) -> bool {
    v.chars().any(|c| c == '\r' || c == '\n' || c == '\0')
}

/// 解析 `headers_json`，返回 `(名字小写, 值)`。
///
/// `Err` 里是**给用户看的中文原因**，会直接显示在保存失败的提示里。
fn parse(raw: &str) -> Result<Vec<(String, String)>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let val: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("自定义请求头不是合法 JSON：{e}"))?;
    let obj = val
        .as_object()
        .ok_or_else(|| "自定义请求头必须是 JSON 对象，形如 {\"X-Title\": \"MyApp\"}".to_string())?;

    let mut out = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        let name = k.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err("自定义请求头里有空的头名字".into());
        }
        // 头名字的合法字符集（RFC 7230 token）。放宽会让 reqwest 在转发时才炸。
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(format!("头名字 `{k}` 含非法字符（只允许字母、数字、`-`、`_`、`.`）"));
        }
        let text = match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => {
                return Err(format!(
                    "头 `{k}` 的值必须是字符串（或数字/布尔），不能是数组或对象"
                ))
            }
        };
        if has_control_char(&text) {
            return Err(format!("头 `{k}` 的值含换行或控制字符，会造成请求头注入"));
        }
        out.push((name, text));
    }
    Ok(out)
}

/// 保存 Key 时的校验：格式不合法、或填了保留字段，一律**拒绝并说明**。
///
/// 挂在 `service.rs::save_key`，与 `reject_desktop_key_with_unusable_model_names`
/// 并列 —— 那是本仓既定的「保存前在源头拦下」挂点。
pub(crate) fn reject_reserved(key: &ProviderKey) -> Result<(), String> {
    let Some(raw) = key.headers_json.as_deref() else {
        return Ok(());
    };
    let pairs = parse(raw)?;
    let bad: Vec<&str> = pairs
        .iter()
        .filter(|(n, _)| is_reserved(n))
        .map(|(n, _)| n.as_str())
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "这些头由 SynaRoute 自己管理，不能自定义：{}。\n\
             它们分别负责鉴权（用本 Key 的密钥）、协议版本、内容长度与压缩协商 —— \n\
             覆盖它们会让请求带着错误的凭据发出去，而上游只会回一句「鉴权失败」，排查不到这里。",
            bad.join("、")
        ));
    }
    Ok(())
}

/// 转发时取该 Key 的自定义头。
///
/// 与 [`reject_reserved`] 的差别：这里**不报错，静默丢掉**不合法/保留的条目。
/// 理由是走到转发时配置已经落盘了，报错也没人看；而带着一个会顶掉真实 Key 的头
/// 发出去，后果比少一个自定义头严重得多。丢的时候记一条 warn 供排障。
pub(crate) fn headers_for(key: &ProviderKey) -> Vec<(String, String)> {
    let Some(raw) = key.headers_json.as_deref() else {
        return Vec::new();
    };
    match parse(raw) {
        Ok(pairs) => {
            let (ok, dropped): (Vec<_>, Vec<_>) =
                pairs.into_iter().partition(|(n, _)| !is_reserved(n));
            if !dropped.is_empty() {
                tracing::warn!(
                    "Key {} 的自定义请求头里有保留字段，已忽略：{}",
                    key.id,
                    dropped
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                );
            }
            ok
        }
        Err(e) => {
            tracing::warn!("Key {} 的自定义请求头无法解析，已整体忽略：{e}", key.id);
            Vec::new()
        }
    }
}

/// 取一份 `.rs` 源码的**生产段**（尾部 `#[cfg(test)] mod tests` 之前的部分），
/// 供各处「源码级判据」使用。
///
/// 🔴 **为什么不能用 `src.split("#[cfg(test)]").next()`**（本仓两条既有源码级判据当初的写法）：
/// 很多文件在**中间**就有 `#[cfg(test)]` 单项（`service.rs:46` 的 notify 桩、
/// `proxy.rs:2746`、`store.rs:2893`），于是切出来的「生产段」只到那一行为止。
///
/// 本轮就真被咬了一次：`save_path_must_call_the_reserved_check` 要找的调用在
/// `service.rs:302`，而朴素切片只到第 45 行 —— 判据红了，而代码是对的。
///
/// 那次是**响亮**的失败。危险的是另一半：对**否定**断言
/// （`assert!(!prod.contains(缺陷形态))`）来说，切片被提前截断 = 断言**空洞通过**
/// —— 判据变绿而缺陷仍在。`lan_guard` 与 `log_rotate` 那两条都是否定断言。
/// 已核过它们**当时并未失效**（`accept` 在 proxy.rs:342，远早于 2746；
/// store.rs 那个形态只出现在测试段），但那是位置的运气，不是判据的性质 ——
/// 代码一挪、或有人在上面加个 `#[cfg(test)]` 单项，就会静默变绿。故统一收到这里。
///
/// 判据取「**最后一个** `#[cfg(test)]` 后面紧跟 `mod tests`」，与
/// `scripts/lib/rust-source.mjs`（棘轮与策略门共用的那份）语义一致。
///
/// 放在本模块是因为 `lib.rs` 棘轮余量为 0、加不了新的 `mod` 声明，而这里有空间。
#[cfg(test)]
pub(crate) fn production_slice(src: &str) -> &str {
    let mut cut = src.len();
    let mut from = 0usize;
    while let Some(i) = src[from..].find("#[cfg(test)]") {
        let at = from + i;
        let rest = &src[at + "#[cfg(test)]".len()..];
        if rest.trim_start().starts_with("mod tests")
            || rest.trim_start().starts_with("pub(crate) mod tests")
        {
            cut = at; // 继续找，取最后一个
        }
        from = at + 1;
    }
    &src[..cut]
}

/// 生产段里**只剩代码的那部分**：先取 [`production_slice`]，再逐行剥掉 `//` 系注释
/// （含 `///` 文档注释与 `//!` 模块注释）。
///
/// 🔴 **凡是「代码里必须/不许出现某个字面量」的判据都得用这个，不能用 `production_slice`。**
///
/// 本仓已经因为这个栽过三次，前两次记在 CLAUDE.md 里
/// （`data-dir-env-name-must-match` 第一版命中脚本自己注释里那段「❌ 已证伪的修法」；
/// `userPrefsParity` 裸 grep `...rest` 命中 `prefs.ts` 注释里**警告不要用 `...rest`** 的句子）。
///
/// 第三次是 `proxy_listen` 的 `only_v6_must_be_set_explicitly`：模块注释里写着
/// 「为什么 v6 socket 必须显式 `set_only_v6(true)`」，于是把**那行代码整个删掉**、
/// 判据**照样绿** —— 注释替代码把断言满足了。注入实测确认（11 条注入里唯一没红的一条）。
///
/// 两个方向都会坏，且都很隐蔽：
/// - 肯定断言（`contains(好形态)`）→ 注释里提到就算通过，**代码没了也不报**；
/// - 否定断言（`!contains(坏形态)`）→ 注释里举反例就算违规，**代码是对的却报红**。
///
/// **判据说「代码里别这么写」，就只能看代码。**
///
/// 剥法刻意从简（按行找 `//`，不解析字符串字面量里的 `//`）：
/// 代价是含 `//` 的字符串（如 `"https://x"`）会被误截。这对本类判据无害 ——
/// 它们找的都是 `foo(` / `bar::baz` 这类标识符形态，不是 URL；
/// 而要真去解析 Rust 词法，这个helper 的复杂度会超过它守护的东西。
#[cfg(test)]
pub(crate) fn production_code_only(src: &str) -> String {
    production_slice(src)
        .lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CategoryType;

    /// 判据自身的判据：中间的 `#[cfg(test)]` 单项不能把生产段截断。
    #[test]
    fn production_slice_ignores_mid_file_cfg_test_items() {
        let src = "a\n#[cfg(test)]\nfn stub() {}\nb\n#[cfg(test)]\nmod tests {\nc\n}\n";
        let prod = production_slice(src);
        assert!(prod.contains("fn stub"), "中间那个单项之后的代码仍属生产段");
        assert!(prod.contains("\nb\n"), "b 必须在生产段里");
        assert!(!prod.contains("\nc\n"), "测试段里的 c 不该在");
    }

    fn key_with(raw: Option<&str>) -> ProviderKey {
        let mut k = crate::model::ProviderKey {
            id: "k1".into(),
            category_id: CategoryType::ClaudeCli,
            ..Default::default()
        };
        k.headers_json = raw.map(|s| s.to_string());
        k
    }

    #[test]
    fn plain_custom_headers_round_trip() {
        let k = key_with(Some(r#"{"HTTP-Referer":"https://x.dev","X-Title":"MyApp"}"#));
        assert!(reject_reserved(&k).is_ok());
        let mut got = headers_for(&k);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("http-referer".to_string(), "https://x.dev".to_string()),
                ("x-title".to_string(), "MyApp".to_string()),
            ],
            "OpenRouter 那两个头是本功能的真实用例"
        );
    }

    /// 🔴 保存时**必须拒绝并说明**，不能静默忽略 —— 静默忽略会让用户以为生效了，
    /// 然后去查中转站/网络，而真正的原因是我们悄悄丢了他的输入。
    #[test]
    fn saving_a_reserved_header_is_rejected_with_an_explanation() {
        for bad in [
            r#"{"Authorization":"Bearer hack"}"#,
            r#"{"x-api-key":"hack"}"#,
            r#"{"Content-Length":"0"}"#,
            r#"{"accept-encoding":"gzip"}"#,
        ] {
            let err = reject_reserved(&key_with(Some(bad))).expect_err(&format!("{bad} 该被拒"));
            assert!(
                err.contains("由 SynaRoute 自己管理"),
                "错误里要说清为什么，不能只说『无效』：{err}"
            );
        }
    }

    /// 转发时那道防线：字段在有 UI 之前就存在，用户可能手改过 config.json。
    /// 放一个 `authorization` 过去会**顶掉真实 Key** → 上游 401，而日志只显示「鉴权失败」。
    #[test]
    fn forwarding_silently_drops_reserved_headers_that_bypassed_the_save_check() {
        let k = key_with(Some(r#"{"authorization":"Bearer hack","X-Title":"ok"}"#));
        let got = headers_for(&k);
        assert_eq!(
            got,
            vec![("x-title".to_string(), "ok".to_string())],
            "保留字段要被丢掉，其余照常生效"
        );
    }

    #[test]
    fn header_values_with_newlines_are_rejected() {
        // CR/LF 会把一行头拆成多行 = 请求头注入。reqwest 也会拒，但报的是
        // 看不出成因的 InvalidHeaderValue，而这里能在保存时就说清。
        let err = reject_reserved(&key_with(Some("{\"X-A\":\"a\\r\\nX-Evil: 1\"}")))
            .expect_err("含 CRLF 的值该被拒");
        assert!(err.contains("注入"), "要点明是注入风险：{err}");
    }

    #[test]
    fn malformed_json_is_rejected_on_save_but_ignored_on_forward() {
        let k = key_with(Some("not json"));
        assert!(reject_reserved(&k).is_err(), "保存时报错");
        assert!(headers_for(&k).is_empty(), "转发时整体忽略、不能panic");

        let arr = key_with(Some(r#"["a","b"]"#));
        assert!(reject_reserved(&arr).unwrap_err().contains("JSON 对象"));

        let nested = key_with(Some(r#"{"X-A":{"b":1}}"#));
        assert!(reject_reserved(&nested).unwrap_err().contains("必须是字符串"));
    }

    #[test]
    fn empty_and_absent_are_both_no_ops() {
        for raw in [None, Some(""), Some("   ")] {
            let k = key_with(raw);
            assert!(reject_reserved(&k).is_ok());
            assert!(headers_for(&k).is_empty());
        }
    }

    #[test]
    fn illegal_header_names_are_rejected() {
        for bad in [r#"{"X A":"v"}"#, r#"{"X:A":"v"}"#, r#"{"":"v"}"#] {
            assert!(reject_reserved(&key_with(Some(bad))).is_err(), "{bad} 该被拒");
        }
    }

    /// 数字/布尔值放过（用户写 `{"X-Retry": 3}` 是自然的），但要转成字符串。
    #[test]
    fn numbers_and_bools_are_coerced_to_strings() {
        let k = key_with(Some(r#"{"X-N":3,"X-B":true}"#));
        let mut got = headers_for(&k);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("x-b".to_string(), "true".to_string()),
                ("x-n".to_string(), "3".to_string()),
            ]
        );
    }

    /// 🔴 **接线判据一**：保存路径必须真的调 `reject_reserved`。
    ///
    /// 上面那些用例都直接调这个函数，于是**把 `service.rs` 里那行删掉它们照样全绿**
    /// —— 而那就是「填了保留字段照样落盘」这个缺陷本身。
    /// 同 `route_meta` / `lan_guard` 的 peer / `log_rotate` 的写线程那几次。
    #[test]
    fn save_path_must_call_the_reserved_check() {
        let prod = production_slice(include_str!("service.rs"));
        assert!(
            prod.contains("custom_headers::reject_reserved(&key)"),
            "service.rs::save_key 必须在落盘前调 reject_reserved"
        );
    }

    /// 🔴 **接线判据二**：转发处必须真的合并自定义头，且必须在**鉴权头之前**。
    ///
    /// 删掉合并循环 → 「配了但完全无效」，而其余用例全绿。
    /// 顺序反了 → 用户能用自定义头顶掉真实 Key（保存校验可被手改 config.json 绕过）。
    #[test]
    fn apply_upstream_headers_must_merge_custom_headers_before_auth() {
        let prod = production_slice(include_str!("proxy.rs"));
        let merge = prod
            .find("custom_headers::headers_for(key)")
            .expect("apply_upstream_headers 必须合并 headers_for(key)");
        let auth = prod
            .find("rb.header(scheme.header_name(), scheme.header_value(secret))")
            .expect("鉴权头那行还在吧");
        assert!(
            merge < auth,
            "自定义头必须合并在鉴权头之前，否则用户能顶掉真实 Key"
        );
    }
}
