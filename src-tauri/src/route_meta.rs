//! 转发诊断响应头（`X-SynaRoute-*`）—— 借鉴 OmniRoute 的 `X-OmniRoute-*` 一组头
//! （`src/domain/omnirouteResponseMeta.ts` + `src/shared/constants/headers.ts`）。
//!
//! ## 为什么值得单独做一个模块
//!
//! 在此之前 SynaRoute **一个自有响应头都没有**：客户端拿到回答，却无从知道这次走了哪条 Key、
//! 切换了几次、实际打的是哪个上游模型名。要查就得回 UI 翻日志。
//!
//! 而对本项目，这组头有第二重、更硬的价值：**它是唯一能从「用户真实进程」取证的通道**。
//! MSIX AppData 虚拟化下（见 CLAUDE.md「平行宇宙」一节），Claude Code 自己启动的实例活在
//! 包内私有副本里、其表现不代表用户；而响应头是**用户的客户端**收到的，不受包身份影响。
//! 让用户贴一段响应头，比让用户描述现象可靠一个数量级。
//!
//! ## 单一 chokepoint
//!
//! 全部头由 [`build_headers`]（纯函数）产生、由 [`attach`] 挂上。转发路径上每一个
//! **返回给下游的出口**都必须走 `attach`——包括失败出口。漏掉一个出口，就有一类请求
//! 永远查不到路由信息，而这种缺失是静默的（没人会因为「少了个头」而报 bug）。
//! 现有出口清单见 `proxy.rs` 中 `route_meta::attach` 的调用点。
//!
//! ## 🔴 这组头允许携带什么（改动前先读）
//!
//! 允许：用户自己起的 Key 名、Key 的 uuid、解析后的上游模型名、若干整数、请求 uuid。
//!
//! **禁止**，且每条都有具体理由：
//! - **`base_url` / 完整上游 URL** —— 部分中转站把访问令牌放在 URL 路径里
//!   （`https://host/v1/<token>/`）。把 URL 写进响应头等于把密钥回显给下游。
//!   `RouteMeta` 结构里**故意不留** url 字段，让这件事在编译期就做不到。
//! - **密钥本身**（同上，且 `RouteMeta` 无此字段）。
//! - **错误消息 / 上游错误体 / 栈** —— 上游错误体常整段回显请求内容。错误信息走响应体
//!   （那里已有既定的截断与脱敏口径），不进头。头里只放 `upstream_status` 这个整数。
//!
//! 新增头时**必须**同步更新 `header_name_set_is_frozen` 那条测试——它冻结了对外暴露的
//! 头名全集，加头就会变红，强制过一遍上面这份「允许/禁止」清单。这是刻意的摩擦。

use hyper::http::header::{HeaderName, HeaderValue};
use hyper::Response;

/// 头名常量。**必须全小写**：`HeaderName::from_static` 对大写会 panic
/// （HTTP 头名大小写不敏感，客户端侧照常显示为 `X-SynaRoute-...`）。
pub mod names {
    pub const VERSION: &str = "x-synaroute-version";
    pub const REQUEST_ID: &str = "x-synaroute-request-id";
    pub const KEY: &str = "x-synaroute-key";
    pub const KEY_ID: &str = "x-synaroute-key-id";
    pub const MODEL: &str = "x-synaroute-model";
    pub const ATTEMPTS: &str = "x-synaroute-attempts";
    pub const LATENCY_MS: &str = "x-synaroute-latency-ms";
    pub const UPSTREAM_STATUS: &str = "x-synaroute-upstream-status";
    pub const DECISION: &str = "x-synaroute-decision";
}

/// 一次转发的路由结论。字段只增不改语义；**不要**加 url / 密钥字段（见模块注释）。
#[derive(Debug, Clone, Default)]
pub struct RouteMeta {
    /// 本次请求的 uuid。同时写进请求日志的 trace，供「响应头 → 日志条目」对账。
    pub request_id: String,
    /// 命中的 Key 名（用户自己起的标签，可含中文/空格）。
    pub key_name: String,
    /// 命中的 Key id（uuid）。名字可重复、可改，id 才是稳定锚点。
    pub key_id: String,
    /// 映射后实际发往上游的模型名。客户端要的名字它自己知道，它不知道的是这个。
    pub real_model: String,
    /// 一共尝试了几条 Key（1 = 首选就成了）。> 1 即说明发生过故障转移。
    pub attempts: u32,
    /// 本次尝试耗时（毫秒）。失败出口上是最后一次尝试的耗时。
    pub latency_ms: u64,
    /// 上游 HTTP 状态码。成功出口不填（下游看得到自己的状态码）；
    /// 失败出口填**最后一次**上游状态码——这是「代理自己造成的失败」与
    /// 「上游真的挂了」最快的区分依据。连接层失败无状态码，留 None。
    pub upstream_status: Option<u16>,
}

/// 头值里绝不允许出现的字符：ASCII 控制字符 + DEL。
/// 它们能把一个头拆成两个（CR/LF 响应拆分），必须先剥掉再谈其它。
fn strip_control_chars(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{7f}')
        .collect()
}

/// 可见 ASCII（0x20..=0x7e）之外的字节一律 percent-encode。
///
/// 自己写而不引 `percent-encoding`：只有这一处用，且逻辑短到没有出错空间。
/// 编码口径同 `encodeURIComponent` 的未保留集（`A-Za-z0-9-_.~`），
/// 因此下游可以直接用 `decodeURIComponent` 还原。
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for b in value.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// 把任意字符串变成合法头值。
///
/// 与 OmniRoute 的 `toHeaderValue()` 的差别（刻意的）：它对整条已拼好的复合串做编码，
/// 于是一个中文 Key 名会把 `key=…; model=…` 里的 `=`、`;`、空格全都编码掉，整条头变得不可读。
/// 这里改为**逐字段**先净化再拼接，结构分隔符永远是字面量，只有越界的那个值被编码。
pub fn to_header_value(value: &str) -> String {
    let cleaned = strip_control_chars(value);
    if cleaned.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        cleaned
    } else {
        percent_encode(&cleaned)
    }
}

/// 构造这次转发要挂的全部头（纯函数，便于单测；`attach` 只负责往 `Response` 上装）。
///
/// 空字符串字段一律**省略**而不是发一个空头：空头对客户端是噪声，且会让
/// 「这个字段本次不适用」和「这个字段是空值」两种情况无法区分。
pub fn build_headers(meta: &RouteMeta) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::with_capacity(9);

    out.push((names::VERSION, env!("CARGO_PKG_VERSION").to_string()));

    if !meta.request_id.is_empty() {
        out.push((names::REQUEST_ID, to_header_value(&meta.request_id)));
    }
    if !meta.key_name.is_empty() {
        out.push((names::KEY, to_header_value(&meta.key_name)));
    }
    if !meta.key_id.is_empty() {
        out.push((names::KEY_ID, to_header_value(&meta.key_id)));
    }
    if !meta.real_model.is_empty() {
        out.push((names::MODEL, to_header_value(&meta.real_model)));
    }
    // attempts / latency 无条件发：0 与 1 是有意义的区别（0 = 一条候选都没试上），
    // 而「头不存在」会被读成「这版没这个功能」。
    out.push((names::ATTEMPTS, meta.attempts.to_string()));
    out.push((names::LATENCY_MS, meta.latency_ms.to_string()));
    if let Some(status) = meta.upstream_status {
        out.push((names::UPSTREAM_STATUS, status.to_string()));
    }

    if let Some(decision) = build_decision_value(meta) {
        out.push((names::DECISION, decision));
    }
    out
}

/// 复合结论头：`key=<名>; model=<上游模型>; attempts=<n>; latency_ms=<n>[; upstream_status=<n>]`。
///
/// 存在的理由是「一行可粘贴」：让用户贴一条就够，不必挨个复制 6 个头。
/// key 名与模型名都空（例如一条候选都没进到转发）时返回 None——
/// 那时这条头只剩两个整数，不如不发。
fn build_decision_value(meta: &RouteMeta) -> Option<String> {
    if meta.key_name.is_empty() && meta.real_model.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::with_capacity(5);
    if !meta.key_name.is_empty() {
        parts.push(format!("key={}", to_header_value(&meta.key_name)));
    }
    if !meta.real_model.is_empty() {
        parts.push(format!("model={}", to_header_value(&meta.real_model)));
    }
    parts.push(format!("attempts={}", meta.attempts));
    parts.push(format!("latency_ms={}", meta.latency_ms));
    if let Some(status) = meta.upstream_status {
        parts.push(format!("upstream_status={status}"));
    }
    Some(parts.join("; "))
}

/// **单一 chokepoint**：把诊断头挂到即将返回下游的响应上。
///
/// 泛型在 body 上（而非绑定 `proxy::ResBody`）：出口有 `full_body` 与流式两种 body 类型，
/// 且这样本模块不必反向依赖 proxy。
///
/// 头值构造不出合法 `HeaderName`/`HeaderValue` 时**静默跳过该条**而不是 panic：
/// 诊断头永远不值得把一个本来成功的转发弄崩。`to_header_value` 已保证正常路径下不会走到。
pub fn attach<B>(resp: &mut Response<B>, meta: &RouteMeta) {
    let headers = resp.headers_mut();
    for (name, value) in build_headers(meta) {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.insert(n, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> RouteMeta {
        RouteMeta {
            request_id: "11111111-2222-3333-4444-555555555555".into(),
            key_name: "Sub2API".into(),
            key_id: "k-abc".into(),
            real_model: "claude-opus-5".into(),
            attempts: 2,
            latency_ms: 842,
            upstream_status: None,
        }
    }

    fn get<'a>(hs: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        hs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    /// 冻结对外暴露的头名全集。
    ///
    /// 这条测试的用途不是「验证功能」，而是**制造摩擦**：新增一个头会让它变红，
    /// 迫使加头的人回到模块注释里那份「允许/禁止」清单过一遍。
    /// 历史上这个项目最贵的几个缺陷都是「悄悄多带了/少带了一样东西」。
    #[test]
    fn header_name_set_is_frozen() {
        let mut names: Vec<&str> = build_headers(&RouteMeta {
            upstream_status: Some(429),
            ..meta()
        })
        .into_iter()
        .map(|(n, _)| n)
        .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "x-synaroute-attempts",
                "x-synaroute-decision",
                "x-synaroute-key",
                "x-synaroute-key-id",
                "x-synaroute-latency-ms",
                "x-synaroute-model",
                "x-synaroute-request-id",
                "x-synaroute-upstream-status",
                "x-synaroute-version",
            ],
            "新增/删除诊断头请先读 route_meta 模块注释的「允许携带什么」清单，再更新本断言"
        );
    }

    /// 头名必须全小写：`HeaderName::from_static` 对大写字母 panic，
    /// 而 `attach` 里用的是 `from_bytes`（不 panic、只是静默失败）——
    /// 那种失败形态最难发现，故在这里挡住。
    #[test]
    fn header_names_are_lowercase_and_parseable() {
        for (name, _) in build_headers(&meta()) {
            assert_eq!(name, name.to_ascii_lowercase(), "头名必须全小写: {name}");
            assert!(
                HeaderName::from_bytes(name.as_bytes()).is_ok(),
                "头名不是合法 HeaderName: {name}"
            );
        }
    }

    #[test]
    fn decision_header_has_pasteable_single_line_shape() {
        let hs = build_headers(&meta());
        assert_eq!(
            get(&hs, names::DECISION),
            Some("key=Sub2API; model=claude-opus-5; attempts=2; latency_ms=842")
        );
    }

    #[test]
    fn upstream_status_is_appended_only_when_present() {
        let hs = build_headers(&meta());
        assert_eq!(get(&hs, names::UPSTREAM_STATUS), None);
        assert!(!get(&hs, names::DECISION).unwrap().contains("upstream_status"));

        let hs = build_headers(&RouteMeta { upstream_status: Some(529), ..meta() });
        assert_eq!(get(&hs, names::UPSTREAM_STATUS), Some("529"));
        assert!(get(&hs, names::DECISION).unwrap().ends_with("upstream_status=529"));
    }

    /// 控制字符必须被剥掉：CR/LF 能把一个头拆成两个（响应头注入）。
    /// Key 名是用户自由输入，粘贴带换行的文本完全可能。
    #[test]
    fn control_chars_are_stripped_so_a_key_name_cannot_split_the_header() {
        let hs = build_headers(&RouteMeta {
            key_name: "evil\r\nx-injected: 1".into(),
            ..meta()
        });
        let v = get(&hs, names::KEY).unwrap();
        assert!(!v.contains('\r') && !v.contains('\n'), "残留换行: {v:?}");
        assert_eq!(v, "evilx-injected: 1");
        // 复合头同样不能被拆开
        assert!(!get(&hs, names::DECISION).unwrap().contains('\n'));
    }

    /// 非 ASCII 逐字段编码，**结构分隔符保持字面量**。
    /// 这是与 OmniRoute 的实现差异点（它整条编码，中文 Key 名会让整行不可读）。
    #[test]
    fn non_ascii_is_percent_encoded_but_separators_stay_readable() {
        let hs = build_headers(&RouteMeta { key_name: "林夕公益站".into(), ..meta() });
        let key = get(&hs, names::KEY).unwrap();
        assert!(key.starts_with('%') && key.is_ascii(), "未编码: {key}");

        let decision = get(&hs, names::DECISION).unwrap();
        assert!(decision.is_ascii());
        // 分隔符没有被一起编码掉（`=` 若被编码会变成 %3D，`; ` 会变成 %3B%20）
        assert!(decision.contains("; model=claude-opus-5"), "结构被编码破坏: {decision}");
        assert!(!decision.contains("%3D") && !decision.contains("%3B"));
    }

    /// 每个头值都必须是合法 `HeaderValue`——否则 `attach` 会静默丢掉它。
    /// 用一组恶意/极端输入过一遍。
    #[test]
    fn every_built_value_is_a_valid_header_value() {
        for bad in [
            "林夕公益站",
            "a\r\nb",
            "\u{7f}del",
            "tab\there",
            "emoji🚀",
            "\u{0}nul",
            "  ",
        ] {
            for (name, value) in build_headers(&RouteMeta {
                key_name: bad.to_string(),
                real_model: bad.to_string(),
                request_id: bad.to_string(),
                key_id: bad.to_string(),
                ..meta()
            }) {
                assert!(
                    HeaderValue::from_str(&value).is_ok(),
                    "{name} 的值不是合法 HeaderValue（输入 {bad:?} → {value:?}）"
                );
            }
        }
    }

    /// 空字段省略而不是发空头。
    #[test]
    fn empty_fields_are_omitted_not_sent_blank() {
        let hs = build_headers(&RouteMeta {
            attempts: 0,
            latency_ms: 0,
            ..Default::default()
        });
        assert_eq!(get(&hs, names::KEY), None);
        assert_eq!(get(&hs, names::MODEL), None);
        assert_eq!(get(&hs, names::REQUEST_ID), None);
        assert_eq!(get(&hs, names::DECISION), None, "无 key 名与模型名时不该发复合头");
        // 但版本与两个计数恒发（见 build_headers 注释）
        assert_eq!(get(&hs, names::ATTEMPTS), Some("0"));
        assert_eq!(get(&hs, names::LATENCY_MS), Some("0"));
        assert!(get(&hs, names::VERSION).is_some());
    }

    /// 版本号取自 Cargo，不是手写常量——手写必然与 `Cargo.toml` 漂移。
    #[test]
    fn version_header_comes_from_cargo_not_a_literal() {
        let hs = build_headers(&meta());
        assert_eq!(get(&hs, names::VERSION), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn attach_puts_every_built_header_on_the_response() {
        let mut resp = Response::new(());
        let m = RouteMeta { upstream_status: Some(429), ..meta() };
        attach(&mut resp, &m);
        for (name, value) in build_headers(&m) {
            assert_eq!(
                resp.headers().get(name).map(|v| v.to_str().unwrap()),
                Some(value.as_str()),
                "{name} 没挂上"
            );
        }
    }

    /// `attach` 不得动响应已有的头（content-type 等）。
    #[test]
    fn attach_does_not_clobber_existing_headers() {
        let mut resp = Response::new(());
        resp.headers_mut()
            .insert("content-type", HeaderValue::from_static("text/event-stream"));
        attach(&mut resp, &meta());
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
    }
}
