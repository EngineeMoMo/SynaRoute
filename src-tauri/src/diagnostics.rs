//! 诊断报告（UX#12）：把排障要用的东西汇成一个纯文本，供用户报障时附上。
//!
//! 从 service.rs 抽出来的：它是一段完全自洽的纯逻辑（入参 Store + DiagnosticsEnv，
//! 出参一个 String），与 service.rs 其余那些「编排代理 / MCP / 客户端配置」的异步流程毫无耦合。
//! 抽出的直接动因是棘轮（service.rs 冻结在 1608、余量为 0，而本轮要往里加
//! `expected_endpoint`），但这一段本来就该独立：这里最该被测的是**脱敏** ——
//! 报告是用户要发给别人的，漏一个密钥字段就是一次真实泄露。

use crate::model::CategoryType;
use crate::store::Store;

/// 遮掉 URL 里可能就是凭据的部分：`user:pass@`、像令牌的路径段、全部 query 值。
///
/// # 🔴 为什么需要它（审查发现）
///
/// 本文件开头对用户声明「本文件**不**包含：任何 API 密钥明文」，而 `base_url` 此前
/// **原样打印两次**（Key 摘要行 + 「配置（已脱敏）」那段 JSON）—— 那份脱敏只按**键名**
/// 与 `sk-` 前缀判，`baseUrl` 不在清单里。
///
/// 而同仓 [`crate::route_meta`] 为**同一个字段**立的规矩正好相反，原文：
/// 「部分中转站把访问令牌放在 URL 路径里（`https://host/v1/<token>/`）。把 URL 写进响应头
/// 等于把密钥回显给下游。`RouteMeta` 结构里**故意不留** url 字段，让这件事在编译期就做不到。」
///
/// 同一条风险，一处编译期禁掉、另一处写进用户要发给别人的报告 —— 而报告开头还向他保证不含密钥。
///
/// # 为什么不整段遮掉
///
/// 原注释说得对：`base_url` 常是问题根源（协议选错、路径写错），整段遮掉会拿走这份诊断价值。
/// 故只遮**像凭据**的部分，`https://api.example.com/v1` 这类原样保留。
///
/// # 判据：长且不含点的 opaque 段
///
/// 真实路径段是词（`v1`/`chat`/`completions`/`openai`）或带点的域名式片段；令牌是 ≥16 位的
/// 无意义串。**宁可多遮一个长段，也不能漏一个令牌** —— 前者只是报告里少一行线索，
/// 后者是把用户的付费凭据发给了他正在求助的那个人。
fn mask_url_credentials(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (format!("{s}://"), r),
        None => (String::new(), url),
    };
    // query 一律只留键名：`?key=xxx` / `?token=xxx` 是另一种常见放法。
    let (path_part, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let mut segs = path_part.split('/');
    // 第一段是 host（可能带 `user:pass@`）。userinfo 里的密码是明确的凭据。
    let host = segs.next().unwrap_or("");
    let host = match host.rsplit_once('@') {
        Some((_userinfo, h)) => format!("***@{h}"),
        None => host.to_string(),
    };
    let mut out = format!("{scheme}{host}");
    for seg in segs {
        out.push('/');
        if looks_like_token(seg) {
            out.push_str("***");
        } else {
            out.push_str(seg);
        }
    }
    if let Some(q) = query {
        out.push('?');
        let masked: Vec<String> = q
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, _)) => format!("{k}=***"),
                None => kv.to_string(),
            })
            .collect();
        out.push_str(&masked.join("&"));
    }
    out
}

/// 路径段是否像令牌：≥16 位、只由 token 字符组成、且**不含点**（域名式片段要留着）。
fn looks_like_token(seg: &str) -> bool {
    seg.len() >= 16
        && !seg.contains('.')
        && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ==== 诊断报告（UX#12）====

/// 诊断报告里那些**只能由 lib.rs 提供**的运行环境信息。
///
/// 为什么单独立个结构：应用版本要 `AppHandle::package_info()`、代理运行状态要
/// `&ProxyManager`。把它们做成入参后，报告拼装本身成了可测的纯逻辑 ——
/// 而这里最该被测的正是**脱敏**：报告是用户要发给别人的，漏一个密钥字段就是真实泄露。
pub(crate) struct DiagnosticsEnv {
    pub(crate) app_version: String,
    pub(crate) exe_path: String,
    /// 各分类代理的 (运行中, 端口)。
    pub(crate) proxy: Vec<(CategoryType, bool, Option<u16>)>,
}

/// 报告里最多附多少条事件。
///
/// 取最后 200 条而非全部：内存日志上限 500 条、单条 detail 可达数百字符，
/// 全带上会让报告膨胀到用户不愿意打开看 —— 而「用户敢不敢发」正是纯文本方案的立足点。
const MAX_EVENTS_IN_REPORT: usize = 200;

/// 拼装诊断报告正文（UX#12）：把排障要用的东西汇成**一个纯文本**，供用户报障时附上。
///
/// **为什么是纯文本而不是 zip**（docs/15 原建议 zip）：
/// 1. 用户能在发出前**亲眼看清里面没有密钥** —— 这直接决定他敢不敢发。zip 里的东西看不见，
///    要额外解释「我保证脱敏了」，信任成本高得多；
/// 2. 不引入 `zip` 直接依赖；
/// 3. 报障场景下贴一段文本比传附件更顺手。
///
/// **绝不包含**：任何密钥明文（config 走 `redacted_config_json` 脱敏）、
/// trace 正文（调用模型日志的请求/响应体，可达数万字符且含完整对话）。
/// 头部显式列出「包含什么、不含什么」，让用户不必逐行审也能判断。
pub(crate) fn build_diagnostics_report(store: &Store, env: &DiagnosticsEnv) -> String {
    use std::fmt::Write as _;
    let mut r = String::with_capacity(16 * 1024);

    let _ = writeln!(r, "# SynaRoute 诊断报告");
    let _ = writeln!(
        r,
        "生成时间：{}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z")
    );
    let _ = writeln!(r);
    let _ = writeln!(r, "## 本文件包含什么");
    let _ = writeln!(r, "- 版本、运行环境、各路径（供核对 MSIX 虚拟化导致的「平行宇宙」问题）");
    let _ = writeln!(r, "- 配置（**已脱敏**：所有密钥字段替换为 ***）");
    let _ = writeln!(r, "- 各 Key 的健康状态与代理运行状态");
    let _ = writeln!(r, "- 最近的事件日志摘要");
    let _ = writeln!(r);
    let _ = writeln!(r, "## 本文件**不**包含");
    let _ = writeln!(r, "- 任何 API 密钥明文");
    let _ = writeln!(r, "- 对话正文（「调用模型日志」的请求体/响应体一律不含）");
    let _ = writeln!(r);

    let _ = writeln!(r, "## 环境");
    let _ = writeln!(r, "- 应用版本：{}", env.app_version);
    let _ = writeln!(
        r,
        "- 操作系统：{} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(r, "- 当前 exe：{}", env.exe_path);
    // 路径是 MSIX 虚拟化问题的关键证据：用户双击启动与被包内进程启动看到的是不同副本。
    let _ = writeln!(r, "- 配置文件：{}", store.config_path_display());
    let _ = writeln!(r, "- 日志目录：{}", store.effective_log_dir().display());
    let _ = writeln!(r, "- 丢弃日志条数（队列满/磁盘慢）：{}", store.log_dropped_count());
    // 第二条丢日志路径，必须单独打。合成一个数字会让排障方向丢失（写得太慢 ≠ 压根写不了），
    // 而**不打**它则让上面那行在盘满时读 0 —— 那是最坏的：日志正在丢而报告说没丢。
    let _ = writeln!(
        r,
        "- 丢弃日志条数（打不开文件，如盘满/目录不可写）：{}",
        crate::store::log_rotate::open_failed_line_count()
    );
    // 第三条：文件打开成功、写入/冲刷失败（卷写满或掉线）。上面两行在这条路径上都读 0，
    // 而「报告说没丢、日志正在丢」是这三行存在的全部理由。
    let _ = writeln!(
        r,
        "- 丢弃日志条数（写入失败，如卷已满/已掉线）：{}",
        crate::store::log_rotate::write_failed_line_count()
    );
    // 状态推送也可能丢（队列满）。丢了不影响正确性——前端 30s 兜底轮询会追上——
    // 但「界面偶尔慢半拍」的排障线索就在这个数字里，必须能被问到。
    let _ = writeln!(r, "- 丢弃状态推送数（队列满）：{}", crate::events::dropped_count());
    // 局域网被拒次数。事件按 IP 去重（防扫描器刷屏），故**只有这个数字**能回答
    // 「有没有人在反复撞令牌」—— 打 1 次和打 50 万次在事件里长得一模一样。
    // 只在非 0 时打：绝大多数用户压根没开局域网，多一行恒 0 的数字是噪音。
    let denied = crate::proxy::lan_guard::denied_count();
    if denied > 0 {
        let _ = writeln!(r, "- 局域网请求被拒次数（未带正确令牌）：{denied}");
    }
    // MCP stdio 子进程的诊断日志。**必须在报告里点名，否则这份新增的可观测性到不了
    // 排障者手上**：用户交出来的是诊断报告和 `logs/*.jsonl`，而它既不在其中、
    // 也不在任何界面/文档里；更糟的是上面那行「日志目录」在用户改过 logDir 时指向别处。
    // 只在文件存在时打（同 denied_count 的取舍：恒 0/恒不存在的一行是噪音）。
    if let Some(p) = crate::mcp::stdio::diag_dir().map(|d| d.join("mcp-stdio.log")) {
        if let Ok(m) = std::fs::metadata(&p) {
            let _ = writeln!(r, "- MCP stdio 子进程日志：{}（{} 字节）", p.display(), m.len());
        }
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "## 代理状态");
    for (cat, running, port) in &env.proxy {
        let _ = writeln!(
            r,
            "- {}: {} 端口={:?}",
            cat.as_str(),
            if *running { "running" } else { "stopped" },
            port
        );
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "## Key 健康状态（不含密钥）");
    for cat in CategoryType::ALL {
        let keys = store.list_keys(cat);
        if keys.is_empty() {
            continue;
        }
        let _ = writeln!(r, "### {}", cat.as_str());
        for k in keys {
            let _ = writeln!(
                r,
                "- [{}] {} | 协议={:?} | 优先级={} | 启用={} | 允许聚合={} | 余额={} | 有密钥={} | 状态={:?} 失败计数={} 熔断至={:?} 延迟={:?}ms | 模型数={} 映射数={}",
                k.id,
                k.name,
                k.protocol,
                k.priority,
                k.enabled,
                // 必须打：否则「一条显示已禁用的 Key 为什么在消耗额度 / 出现在聚合日志里」
                // 这个问题，摘要行给出的答案是「它是禁用的」—— 指向完全错误的方向。
                k.allow_in_aggregate,
                // 同上一条的理由：余额闸门会把「已耗尽」的 Key 降到候选池末尾，而候选顺序
                // 在界面上压根没有呈现。没有这一位，「这条 Key 为什么最后才被用」无从解释。
                crate::health::balance_gate::verdict_label(&k),
                k.has_secret,
                k.health.status,
                k.health.fail_count,
                k.health.breaker_until,
                k.health.latency_ms,
                k.models.len(),
                k.mappings.len()
            );
            // base_url 单列一行：它常是问题根源（协议选错、路径写错）。
            // **必须过 `mask_url_credentials`** —— 中转站会把令牌放在 URL 路径里，
            // 而本报告开头向用户保证「不含任何 API 密钥明文」。理由全文见该函数。
            let _ = writeln!(r, "  base_url: {}", mask_url_credentials(&k.base_url));
        }
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "## 配置（已脱敏）");
    let _ = writeln!(r, "```json");
    match store.redacted_config_json() {
        Ok(s) => {
            // redacted_config_json 只管键名与 sk- 前缀，不碰 base_url —— 那要另外遮。
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(keys) = v.get_mut("keys").and_then(|k| k.as_array_mut()) {
                    for k_val in keys {
                        if let Some(base_url_val) = k_val.get_mut("baseUrl") {
                            if let Some(url_str) = base_url_val.as_str() {
                                *base_url_val = serde_json::Value::String(mask_url_credentials(url_str));
                            }
                        }
                    }
                }
                let _ = writeln!(r, "{}", serde_json::to_string_pretty(&v).unwrap_or(s));
            } else {
                let _ = writeln!(r, "{s}");
            }
        }
        Err(e) => {
            let _ = writeln!(r, "（读取配置失败：{e}）");
        }
    }
    let _ = writeln!(r, "```");
    let _ = writeln!(r);

    let events = store.list_all_events();
    let total = events.len();
    let _ = writeln!(
        r,
        "## 最近事件（共 {total} 条，取最后 {}；**不含**调用模型日志的请求/响应正文）",
        MAX_EVENTS_IN_REPORT.min(total)
    );
    for e in events.iter().rev().take(MAX_EVENTS_IN_REPORT).rev() {
        let ts = chrono::DateTime::from_timestamp_millis(e.ts)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| e.ts.to_string());
        let _ = writeln!(
            r,
            "[{ts}] {} {} {}{}",
            e.category_id.as_str(),
            e.kind,
            e.detail,
            if e.repeat > 1 {
                format!(" (×{})", e.repeat)
            } else {
                String::new()
            }
        );
    }

    // **整份再过一遍脱敏**，而不是只脱敏「配置」那一段。
    //
    // 原先只有配置段走了 `redacted_config_json`，而报告里还有两处会打印用户输入的自由文本：
    // Key 健康状态段的 `name` / `base_url`，以及事件日志的 `detail`。用户把 Key 命名成
    // 含 token 的串（贴错、图省事）并不离谱，那时报告里就有一份未脱敏的凭据 ——
    // 而这份文件的全部意义是「用户敢直接发给别人」，任何一处漏了都等于泄露。
    //
    // 在出口统一处理，日后新增的段落自动被覆盖：这是「默认安全」而非「记得加就安全」。
    // 脱敏是幂等的（替换成 `***`，不含可再匹配的形态），故与配置段那次不冲突。
    crate::tools::redact_config_secrets(&r)
}

/// 诊断报告的默认文件名（带时间戳，避免多次导出互相覆盖）。
pub(crate) fn diagnostics_file_name() -> String {
    format!(
        "synaroute-diagnostics-{}.txt",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // 夹具复用 service.rs 测试段里那两个（`temp_store` 造一个临时目录的 Store、
    // `key` 造一条最小 ProviderKey）。**刻意不在这里再抄一份**：夹具抄两份之后，
    // 「ProviderKey 加了字段」这类改动会让两处各自漂移，而漂移的表现是
    // 「一边测的还是旧结构」——比重复本身更糟。
    use crate::service::tests::{key, temp_store};

    #[test]
    fn mask_url_credentials_preserves_normal_paths() {
        assert_eq!(mask_url_credentials("https://api.example.com/v1"), "https://api.example.com/v1");
        assert_eq!(mask_url_credentials("https://api.example.com/v1/chat/completions"), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn mask_url_credentials_hides_userinfo() {
        assert_eq!(mask_url_credentials("https://user:pass@api.example.com/v1"), "https://***@api.example.com/v1");
    }

    #[test]
    fn mask_url_credentials_hides_token_segments() {
        assert_eq!(
            mask_url_credentials("https://api.example.com/v1/sk-proj-abc123def456ghi789jkl012mno345pqr678stu901vwx234yz/chat"),
            "https://api.example.com/v1/***/chat"
        );
        assert_eq!(
            mask_url_credentials("https://relay.example.com/v1/eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9/completions"),
            "https://relay.example.com/v1/***/completions"
        );
    }

    #[test]
    fn mask_url_credentials_hides_query_values() {
        assert_eq!(
            mask_url_credentials("https://api.example.com/v1?key=secret123&token=abc"),
            "https://api.example.com/v1?key=***&token=***"
        );
    }

    #[test]
    fn mask_url_credentials_preserves_dotted_segments() {
        // 域名式片段（带点）不该被当成令牌遮掉
        assert_eq!(
            mask_url_credentials("https://api.example.com/v1/some.qualified.name/chat"),
            "https://api.example.com/v1/some.qualified.name/chat"
        );
    }

    #[test]
    fn looks_like_token_recognizes_opaque_long_segments() {
        assert!(looks_like_token("sk-proj-abc123def456ghi789jkl012mno345pqr"));
        assert!(looks_like_token("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"));
        assert!(!looks_like_token("v1"));
        assert!(!looks_like_token("chat"));
        assert!(!looks_like_token("completions"));
        assert!(!looks_like_token("some.qualified.name"));
    }

    /// 诊断报告**绝不能含密钥明文**，且必须自带「包含/不含什么」的声明。
    ///
    /// 这是报告能被用户放心发出去的唯一前提。
    ///
    /// 判据取的是**真实泄露路径**：转发失败的事件 detail 里带着上游响应体前若干字符，
    /// 而上游报鉴权失败时把收到的 key 回显在响应体里是很常见的；同理 Key 的名字
    /// 也可能被用户贴成一串 token。这两处都是自由文本，早先只有「配置」那一段过了脱敏，
    /// 它们原样进报告。
    ///
    /// 反过来说，这条测试**不**用 `config.json` 里的字段做判据：那份文件本就不存
    /// API key（密钥在 `secrets.enc` 里，报告压根不读它），拿它断言会得到一条假绿的测试。
    #[test]
    fn diagnostics_report_never_leaks_a_secret() {
        let (store, dir) = temp_store("diag");
        const LEAKED: &str = "sk-upstream-echoed-this-back";
        let mut k = key(CategoryType::ClaudeCli);
        k.name = format!("误贴成密钥的名字 {LEAKED}");
        store.upsert_key(k).unwrap();
        // 模拟转发失败事件：上游 401 的响应体里回显了我们发过去的 key。
        store.append_event(
            CategoryType::ClaudeCli,
            "error",
            Some("k1"),
            &format!("上游 HTTP 401: {{\"error\":\"invalid key {LEAKED}\"}}"),
        );

        let env = DiagnosticsEnv {
            app_version: "9.9.9".into(),
            exe_path: "C:\\test\\synaroute.exe".into(),
            proxy: vec![(CategoryType::ClaudeCli, true, Some(8787))],
        };
        let report = build_diagnostics_report(&store, &env);

        assert!(
            !report.contains(LEAKED),
            "报告里出现了密钥明文——这是直接的凭据泄露。报告全文:\n{report}"
        );
        assert!(report.contains("sk-***"), "应被替换为掩码而非整段删掉（否则排障丢上下文）");
        assert!(report.contains("本文件**不**包含"), "必须自带声明，用户才敢发出去");
        assert!(report.contains("任何 API 密钥明文"));
        assert!(report.contains("9.9.9") && report.contains("synaroute.exe"), "环境信息要在");
        // 路径是 MSIX「平行宇宙」问题的关键证据，不能省。
        assert!(report.contains("配置文件："), "必须打印实际配置路径");
        assert!(report.contains("running 端口=Some(8787)"), "代理状态要如实反映");
        assert!(report.contains("上游 HTTP 401"), "脱敏不该把排障信息本身抹掉");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 出口的整份脱敏**必须幂等**，否则它会把配置段搅坏。
    ///
    /// 报告的配置段已经过了一次 `redacted_config_json`，整份再过一次
    /// `redact_config_secrets` —— 这是刻意的双重覆盖（新增段落自动被保护）。
    /// 但前提是第二遍对已脱敏文本无副作用：若它把第一遍留下的 `***` 或 `sk-***`
    /// 再匹配一次、多截掉几个字符，症状会是「报告里的 JSON 少了个引号」这类
    /// 没人会立刻联想到脱敏的怪现象。
    ///
    /// 判据取「再脱敏一次，结果逐字节相同」。这条同时守住了「脱敏顺序被调换」
    /// （例如有人把出口那次挪到配置段之前）不会改变输出。
    #[test]
    fn redacting_an_already_redacted_report_changes_nothing() {
        let (store, dir) = temp_store("diag_idem");
        let mut k = key(CategoryType::ClaudeCli);
        k.name = "sk-abcdefghijklmnop".into(); // 够长，会命中裸 token 扫描
        k.headers_json = Some(r#"{"api_key":"plain-value","note":"keep me"}"#.into());
        store.upsert_key(k).unwrap();
        store.append_event(
            CategoryType::ClaudeCli,
            "error",
            None,
            r#"上游 401 {"error":{"apiKey":"leaked-abc","message":"invalid"}}"#,
        );

        let env = DiagnosticsEnv {
            app_version: "9.9.9".into(),
            exe_path: "C:\\test\\synaroute.exe".into(),
            proxy: vec![(CategoryType::ClaudeCli, false, None)],
        };
        let once = build_diagnostics_report(&store, &env);
        let twice = crate::tools::redact_config_secrets(&once);

        assert_eq!(once, twice, "出口脱敏不幂等 —— 二次运行改动了报告内容");
        // 顺带确认这份样本真的触发了脱敏（否则上面的相等是空断言）。
        assert!(once.contains("***"), "样本应当命中脱敏，否则这条测试什么都没验");
        assert!(!once.contains("sk-abcdefghijklmnop"), "裸 token 应被掩码");
        assert!(!once.contains("leaked-abc"), "事件里的 apiKey 值应被掩码");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
