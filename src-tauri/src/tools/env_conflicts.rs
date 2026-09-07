//! 客户端环境变量冲突检测：**为什么「接入完成」了却一直不走代理**。
//!
//! 参照 CodexPlusPlus 的 `crates/codex-plus-core/src/env_conflicts.rs`，但判据与它**不同**，
//! 差异是取证得出的而不是口味问题 —— 逐条记在下面，免得下一个人照它「补齐」回去。
//!
//! # ① 它只查 `OPENAI_*`，我们主要查 `ANTHROPIC_*`
//!
//! 这不是加一个前缀那么简单，是**方向相反**：
//!
//! - **`ANTHROPIC_*` 对我们是真冲突。** 接入 Claude CLI 时我们把入口写进
//!   `~/.claude/settings.json` 的 `env` 块（`tools.rs:124` 的 `ANTHROPIC_BASE_URL`）。
//!   系统里若另有一个同名变量，它与我们写的值指向不同的地方，而**哪个生效取决于客户端**。
//!   失效形态是「接入提示说成功了、请求却一个都没到代理」，用户完全看不出原因。
//! - **`OPENAI_*` 对我们（几乎）不是冲突。** 实测 codex.exe **0.151.0-alpha.7.2**：
//!   `OPENAI_BASE_URL` 全二进制**只出现 1 次**，紧邻的源文件路径是
//!   `network-proxy\src\credential_broker\providers\openai.rs` —— 那是凭据代理组件，
//!   不是 provider 解析路径。而 `OPENAI_API_KEY` 那 23 处旁边就是 `env_key` /
//!   `experimental_bearer_token` / `requires_openai_auth` 三个字面量，也就是说
//!   **环境变量是经 `env_key` 起作用的**，而我们写的 provider 块**刻意不写 `env_key`**
//!   （`codex.rs:945` 有一条测试钉着这件事）。
//!
//!   所以照它那样把 `OPENAI_API_KEY` 报成冲突，对**每一个装过 OpenAI SDK 的用户**都是假警。
//!   我们把它降成 `notice` 并把话说成条件句 —— 用户手改过 config.toml 用了 `env_key` 时它
//!   才真的顶事，而那种情形我们无从知道。
//!
//! # ② 只报「值与我们写的不一致」的那些，不报「存在」
//!
//! 它的 `EnvConflict` 只带一位 `value_present`。但 `ANTHROPIC_BASE_URL` 恰好等于我们的入口
//! 是完全正常的形态（用户自己配过、或上一次接入留下的），把它报成冲突就是叫用户去删一个
//! **正确**的设置。判据因此是「**指向别处**」。
//!
//! # ③ 目录变量两侧不一致 —— 它刻意排除了 `CODEX_HOME`，而那才是最静默的一种
//!
//! 它的测试原文就写着 `detects_openai_prefixed_conflicts_but_not_codex_home`。
//! 但对我们来说 `CODEX_HOME` / `CODEX_SQLITE_HOME` 有一种**它没有的**失效形态：
//! 用户在系统设置里配了它（进注册表），而 SynaRoute 这个进程是在那之前启动的、
//! 或者被别的东西以干净环境拉起 —— 于是**我们写的 config.toml 和 Codex 读的不是同一个目录**。
//! 表现是「接入成功、Codex 却完全没变化」，而两边各自都「正确」。
//!
//! 🔴 反过来也成立且更隐蔽：进程里有、注册表里没有（比如从某个设了它的终端启动我们），
//! 那么**用户双击启动 Codex 时**读的是默认 `~/.codex`，而我们写的是别处。两个方向都要报。
//!
//! # 值绝不原样外发
//!
//! 这些发现会进诊断报告（用户会发给别人）与界面。名字里带 `TOKEN`/`KEY`/`SECRET`/`PASSWORD`
//! 的一律只报「已设置」，绝不带值 —— 同 `lan_guard` 那条「明文令牌进事件等于同时进了三个
//! 用户会分享出去的地方」。URL 与目录类**要**带值，那正是「指向哪里」这个答案本身；
//! 诊断报告出口那道 `redact_config_secrets` 是第二层防线，不是唯一一层。

use serde::Serialize;

/// 一条发现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvFinding {
    pub name: String,
    /// `process` = 本进程环境里就有；`user` = Windows 用户级（`HKCU\Environment`）。
    pub source: &'static str,
    /// `conflict` = 有依据认为它会顶掉我们写的配置；`notice` = 只在特定前提下才顶事。
    pub severity: &'static str,
    /// 已脱敏的值。凭据类恒为空串（用 [`Self::has_value`] 表示「设了但不给看」）。
    pub value: String,
    pub has_value: bool,
    /// 为什么这一条值得看 —— **说清它会造成什么**，不是重复变量名。
    pub note: String,
    /// 🔴 **这一条能不能用「移除」按钮解决。**
    ///
    /// 不是所有发现都该删：目录变量（`CODEX_HOME` / `CODEX_SQLITE_HOME`）的正确处置是
    /// **对齐两侧**（重启 SynaRoute，或在系统设置里补上），删掉它会**破坏用户自己的 Codex
    /// 布局** —— 那是他刻意配的。
    ///
    /// 这一位是 [`remove_at`] 与前端按钮的**同一个**事实来源。第一版没有它，于是
    /// `directory_mismatches` 报出的条目在界面上带一个「移除它们」按钮，而 `remove_at` 的
    /// 白名单（走 [`classify`]）认不出它 → 点了返回「没有可移除的项」。
    /// 那正是本仓最忌的「界面能点、点了没反应」。
    pub removable: bool,
}

/// 这个名字的值是凭据吗（决定要不要遮掉）。
fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["TOKEN", "KEY", "SECRET", "PASSWORD"].iter().any(|w| n.contains(w))
}

/// 值最多带多少字符进报告/界面。
const MAX_VALUE_CHARS: usize = 120;

fn shown_value(name: &str, raw: &str) -> (String, bool) {
    let v = raw.trim();
    if v.is_empty() {
        return (String::new(), false);
    }
    if is_secret_name(name) {
        return (String::new(), true);
    }
    let clipped: String = v.chars().take(MAX_VALUE_CHARS).collect();
    (clipped, true)
}

/// Claude 侧那些会顶掉 `settings.json` 的 `env` 块的变量。
///
/// 🔴 **前缀家族不能漏**：`ANTHROPIC_DEFAULT_*_MODEL` 有四档（opus/sonnet/haiku/fable），
/// 而接入时 `tools.rs` 只清 `settings.json` 里的那几个键 —— **环境变量版清不掉**。
/// 用前缀而不是枚举，理由同「加一个字段要搜的是它的同族字段名」那条教训。
const ANTHROPIC_EXACT: &[(&str, &str)] = &[
    (
        "ANTHROPIC_AUTH_TOKEN",
        "Claude CLI 的令牌。它与 SynaRoute 写进 settings.json 的占位令牌是两个值，若客户端优先读环境变量，请求会带着这个令牌直连它对应的上游。",
    ),
    (
        "ANTHROPIC_API_KEY",
        "官方密钥。它在时 Claude CLI 可能直连 api.anthropic.com，完全绕过 SynaRoute（表现为「接入成功但日志页一条请求都没有」）。",
    ),
    (
        "ANTHROPIC_MODEL",
        "会顶掉 SynaRoute 写入的模型名，于是应用内选的模型不生效。",
    ),
    (
        "ANTHROPIC_SMALL_FAST_MODEL",
        "会顶掉杂活档的模型名，绕过我们的三档映射。",
    ),
];

/// Codex 的目录变量：两侧不一致时我们与 Codex 读写的不是同一个目录。
const CODEX_DIRS: &[&str] = &["CODEX_HOME", "CODEX_SQLITE_HOME"];

/// 一次检测的入参：把「我们期望的 Claude CLI 入口」传进来，好判「指向别处」。
///
/// 传空串 = 拿不到（代理没起过、端口还没定），那时 `ANTHROPIC_BASE_URL` 一律按 `notice` 报
/// —— **不能因为拿不到期望值就不报**，那正是最需要它的时刻（用户还没接入成功）。
pub(crate) fn detect(expected_base_url: &str) -> Vec<EnvFinding> {
    detect_from(
        &std::env::vars().collect::<Vec<_>>(),
        &read_user_env(),
        expected_base_url,
    )
}

/// 纯函数版本 —— 判据全部在这一层测。
///
/// 🔴 **测试绝不许 `set_var`**：那是进程级的，而套件并行跑 —— `quota_window` 那次全量红 8 条
/// 就是同一个坑（进程级表 + 共用短 id）。所以两份环境都做成入参。
fn detect_from(
    process: &[(String, String)],
    user: &[(String, String)],
    expected_base_url: &str,
) -> Vec<EnvFinding> {
    let mut out = Vec::new();
    for (source, pairs) in [("process", process), ("user", user)] {
        for (name, value) in pairs.iter() {
            if let Some(f) = classify(name, value, source, expected_base_url) {
                out.push(f);
            }
        }
    }
    out.extend(directory_mismatches(process, user));
    // 名字排序，同名时 process 在前（那是「现在就生效」的那一份）。
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.source.cmp(b.source)));
    out.dedup_by(|a, b| a.name == b.name && a.source == b.source);
    out
}

fn classify(
    name: &str,
    value: &str,
    source: &'static str,
    expected_base_url: &str,
) -> Option<EnvFinding> {
    let name = name.trim();
    let (shown, has) = shown_value(name, value);
    let mk = |severity: &'static str, note: String| {
        Some(EnvFinding {
            name: name.to_string(),
            source,
            severity,
            value: shown.clone(),
            has_value: has,
            note,
            // classify 认的都是「多余的设置」，删掉即恢复我们写的那份。
            removable: true,
        })
    };
    // 空值一律不报：`set FOO=` 留下的空变量对客户端没有影响，报它是纯噪音。
    if !has {
        return None;
    }
    if name == "ANTHROPIC_BASE_URL" {
        // 🔴 判据是「指向别处」而不是「存在」—— 等于我们入口是完全正常的形态。
        if !expected_base_url.is_empty() && value.trim().trim_end_matches('/') == expected_base_url.trim_end_matches('/') {
            return None;
        }
        return mk(
            "conflict",
            format!(
                "它把 Claude CLI 指向{}，而 SynaRoute 的入口是 {}。两者哪个生效取决于客户端 —— 若接入完成后请求仍不经过代理，先移除它再试。",
                if shown.is_empty() { "别处".into() } else { format!(" {shown}") },
                if expected_base_url.is_empty() { "本机代理端口".into() } else { expected_base_url.to_string() },
            ),
        );
    }
    if let Some((_, why)) = ANTHROPIC_EXACT.iter().find(|(n, _)| *n == name) {
        return mk("conflict", (*why).to_string());
    }
    if name.starts_with("ANTHROPIC_DEFAULT_") && name.ends_with("_MODEL") {
        return mk(
            "conflict",
            "档位模型的环境变量版。接入时我们只清得掉 settings.json 里的同名键，环境变量清不掉 —— 它会绕过 SynaRoute 的档位映射。".into(),
        );
    }
    if name == "OPENAI_API_KEY" || name == "OPENAI_BASE_URL" {
        // 见模块头 ①：有取证支撑「通常无害」，故不报成 conflict，也不建议默认删。
        return mk(
            "notice",
            "SynaRoute 写的 Codex provider 用 experimental_bearer_token、不读环境变量，所以它通常无害。只有在你手改过 config.toml 用了 env_key 时，它才会顶掉那里的值。".into(),
        );
    }
    None
}

/// 🔴 目录变量两侧不一致 —— **本模块相对 CodexPlusPlus 最实质的一项**（它刻意排除了
/// `CODEX_HOME`）。两个方向都要报，因为两个方向都会让「我们写的」与「Codex 读的」错位：
///
/// - 注册表有、进程没有：用户在系统设置里配过，而我们这个进程没继承到（在那之前启动的，
///   或被某个干净环境拉起）→ **我们写默认 `~/.codex`，Codex 读他配的那个**；
/// - 进程有、注册表没有：我们是从某个设了它的终端启动的 → 我们写他配的那个，
///   而**用户双击启动 Codex 时**读默认 `~/.codex`；
/// - 两侧都有但值不同：同上，且最容易被看成「都配好了」。
///
/// 失效形态在三种下都一样：接入提示说成功、Codex 里完全没变化，而两边各自都「正确」。
fn directory_mismatches(
    process: &[(String, String)],
    user: &[(String, String)],
) -> Vec<EnvFinding> {
    let pick = |pairs: &[(String, String)], want: &str| -> Option<String> {
        pairs
            .iter()
            .find(|(n, _)| n.trim().eq_ignore_ascii_case(want))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let mut out = Vec::new();
    for name in CODEX_DIRS {
        let p = pick(process, name);
        let u = pick(user, name);
        // 归一化只做「去尾部分隔符」：路径大小写在 Windows 上不敏感，但我们不做
        // canonicalize —— 目录可能压根不存在，而那本身不该让判据失效。
        let norm = |s: &Option<String>| {
            s.as_deref().map(|v| v.trim_end_matches(['/', '\\']).to_ascii_lowercase())
        };
        if norm(&p) == norm(&u) {
            continue;
        }
        let note = match (&p, &u) {
            (Some(pv), None) => format!(
                "SynaRoute 这个进程看到 {name}={pv}，而 Windows 用户级环境变量里没有它 —— 你双击启动 Codex 时它读的是默认目录，与我们写入的不是同一处。"
            ),
            (None, Some(uv)) => format!(
                "Windows 用户级环境变量里有 {name}={uv}，而 SynaRoute 这个进程没继承到它（在那之前启动的，或被别的程序以干净环境拉起）—— 我们写的是默认目录，Codex 读的是这个。重启 SynaRoute 即可对齐。"
            ),
            (Some(pv), Some(uv)) => format!(
                "{name} 两侧不一致：本进程看到 {pv}，用户级环境变量里是 {uv}。我们按前者写，Codex 双击启动时按后者读。"
            ),
            (None, None) => continue,
        };
        out.push(EnvFinding {
            name: (*name).to_string(),
            source: if p.is_some() { "process" } else { "user" },
            severity: "conflict",
            value: p.or(u).unwrap_or_default().chars().take(MAX_VALUE_CHARS).collect(),
            has_value: true,
            note,
            // 🔴 目录变量**不给删**：正确处置是对齐两侧，删掉会破坏用户自己配的 Codex 布局。
            removable: false,
        });
    }
    out
}

/// 读 Windows 用户级环境变量（`HKCU\Environment`）。
///
/// 🔴 **只查进程级会漏掉最常见的那种**：用户在「系统属性 → 环境变量」里配过，
/// 而我们的进程是在那之前启动的 —— 那时 `std::env::vars()` 里干干净净，
/// 而用户下次开客户端就会带上它。
#[cfg(windows)]
fn read_user_env() -> Vec<(String, String)> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    let Ok(key) = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ)
    else {
        return Vec::new();
    };
    key.enum_values()
        .filter_map(|r| r.ok())
        // `REG_EXPAND_SZ`（含 `%…%`）也要收：`to_string()` 给的是未展开的原文，
        // 而我们只需要判「有没有、指不指向别处」，不需要展开。
        .map(|(name, v)| (name, v.to_string()))
        .collect()
}

#[cfg(not(windows))]
fn read_user_env() -> Vec<(String, String)> {
    // 非 Windows 没有「用户级环境变量」这个概念（`~/.profile` 之类不是机器可读的单一来源），
    // 故只查进程级。**刻意返回空而不是去解析 shell rc 文件**：那会得到一堆假阳性。
    Vec::new()
}

/// 诊断报告里那一段。返回 `None` = 一条都没有（绝大多数用户的正常形态，不该多一行噪音）。
pub(crate) fn diagnostics_section(expected_base_url: &str) -> Option<String> {
    let s = render_section(&detect(expected_base_url));
    (!s.is_empty()).then_some(s)
}

/// 把发现渲染成报告里那一段。空清单 → 空串（判据在 `render_section(&[])` 上，不必碰真实环境）。
fn render_section(found: &[EnvFinding]) -> String {
    if found.is_empty() {
        return String::new();
    }
    let mut s = String::from("## 客户端环境变量（可能顶掉我们写的配置）\n");
    for f in found {
        let val = if f.value.is_empty() {
            if f.has_value { "（已设置，值不外发）".to_string() } else { String::new() }
        } else {
            format!("={}", f.value)
        };
        s.push_str(&format!(
            "- [{}] {}{} （来源：{}）\n  {}\n",
            f.severity, f.name, val, f.source, f.note
        ));
    }
    s
}

/// 移除的结果。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalResult {
    pub removed: Vec<String>,
    /// 备份文件路径。**没有它就不动手**（见 `remove_at` 的文档）。
    pub backup_path: String,
    /// 逐条失败原因（注册表被策略锁住等）。
    pub failed: Vec<String>,
    pub note: String,
}

/// 备份文件名前缀。
const BACKUP_PREFIX: &str = "env-conflicts";

/// 移除指定的环境变量（用户级 + 本进程），**先备份**。
///
/// 🔴 **默认不自动做，必须用户点。** 这是用户的**系统环境**，删它的影响范围超出本应用 ——
/// 他可能正靠 `ANTHROPIC_API_KEY` 跑别的脚本。所以本函数只在用户明确点了「移除」时被调，
/// 且照 CodexPlusPlus 的做法先写一份 JSON 备份（它的 `remove_env_conflicts` 也是先备份）。
///
/// 🔴 **备份失败一律不动手**（fail closed）：同大脑聚合写文件那条 ——「备份没成还照删」
/// 等于无备份地改用户的系统设置。
///
/// 🔴 **只删我们认得的名字。** 前端传什么就删什么会让这个命令变成一个任意注册表删除原语。
pub(crate) fn remove_at(names: &[String], backup_dir: &std::path::Path) -> crate::error::AppResult<RemovalResult> {
    remove_from(&detect(""), names, backup_dir)
}

/// 纯函数版本：**发现清单做入参**，判据全部在这一层测。
///
/// 🔴 **白名单来自 `detect` 自己报出来的那一份，且只收 `removable` 的。**
///
/// 第一版这里独立走 `classify`，与界面上「哪些条目带移除按钮」是两份判据 —— 于是
/// `directory_mismatches` 报的 `CODEX_HOME` 在界面上能点、点了却被这里拒掉
/// （「没有可移除的项」）。同一个事实两处各判一遍必然漂移，故改为**只信 `removable`**。
///
/// 做成纯函数的第二个理由与 [`detect_from`] 一样：测试**绝不许** `set_var`。
/// 白名单一旦依赖真实环境，用例就得先往进程里塞一个变量 —— 而套件是并行跑的。
fn remove_from(
    found: &[EnvFinding],
    names: &[String],
    backup_dir: &std::path::Path,
) -> crate::error::AppResult<RemovalResult> {
    let allowed: std::collections::HashSet<&str> =
        found.iter().filter(|f| f.removable).map(|f| f.name.as_str()).collect();
    let mut want: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_string())
        .filter(|n| allowed.contains(n.as_str()))
        .collect();
    want.sort();
    want.dedup();
    if want.is_empty() {
        return Ok(RemovalResult {
            note: "没有可移除的项。只有本应用报出来、且该用删除解决的变量才会被移除 ——\
                   目录类（CODEX_HOME 等）不在其中：那要对齐两侧，删掉会破坏你自己的 Codex 布局。"
                .into(),
            ..RemovalResult::default()
        });
    }

    let before: Vec<&EnvFinding> =
        found.iter().filter(|f| want.iter().any(|n| n == &f.name)).collect();
    std::fs::create_dir_all(backup_dir)?;
    let path = backup_dir.join(format!(
        "{BACKUP_PREFIX}-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")
    ));
    // 备份里带的是**发现清单**（已脱敏），不是原始值 —— 凭据类的值我们从头到尾没读进来过。
    // 也就是说这份备份能回答「当时有哪些、指向哪里」，但不能拿来一键恢复一个 token。
    // 那是刻意的：把用户的 token 落到磁盘上另一个文件里，是新增一处泄露面。
    std::fs::write(&path, serde_json::to_vec_pretty(&before)?)?;

    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for name in &want {
        match remove_user_env_value(name) {
            Ok(true) => {
                // 本进程那份也去掉，免得我们自己 spawn 的子进程（MCP stdio）继续继承它。
                std::env::remove_var(name);
                removed.push(name.clone());
            }
            // 平台上没有「用户级环境变量」这个概念（非 Windows）—— **不能报成已移除**。
            // 报成功而实际什么都没持久化，是最坏的一种界面撒谎：用户重启客户端后问题照旧，
            // 而他已经相信这一步做完了。
            Ok(false) => failed.push(format!("{name}：本平台无法持久移除用户级环境变量")),
            Err(e) => failed.push(format!("{name}：{e}")),
        }
    }
    if !removed.is_empty() {
        broadcast_env_change();
    }
    Ok(RemovalResult {
        note: "已改的是「用户级环境变量」。已经在运行的程序（包括 Codex / Claude Code）仍持有旧值，要重启它们才生效。".into(),
        removed,
        backup_path: path.display().to_string(),
        failed,
    })
}

#[cfg(windows)]
fn remove_user_env_value(name: &str) -> std::io::Result<bool> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_SET_VALUE)?;
    match key.delete_value(name) {
        Ok(()) => Ok(true),
        // 只在进程级有、用户级没有时走到这 —— 那不是失败，但也**没有**持久移除任何东西。
        // 返回 `true` 是对的：进程级那一份下面会被删掉，而这次操作确实达成了目的。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

/// 非 Windows 没有「用户级环境变量」这个单一可写来源（`~/.profile` 之类不是）。
/// 返回 `false` 让调用方**如实报「没能移除」**，而不是假装成功。
#[cfg(not(windows))]
fn remove_user_env_value(_name: &str) -> std::io::Result<bool> {
    Ok(false)
}

/// 广播 `WM_SETTINGCHANGE`，让 Explorer 重读环境块。
///
/// 🔴 **少了这一步「移除」按钮就是个空动作**：Explorer 缓存着一份环境块，用户从开始菜单/
/// 桌面启动 Codex 时继承的是那份缓存 —— 于是删完、重启客户端、问题照旧，而用户会得出
/// 「SynaRoute 说删了但没删」这个结论（比不提供这个按钮更糟）。
#[cfg(windows)]
fn broadcast_env_change() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let section: Vec<u16> = "Environment\0".encode_utf16().collect();
    // SAFETY：只读入参、不返回指针；超时 100ms + SMTO_ABORTIFHUNG 保证挂死的窗口拖不住我们
    // （这是在用户点按钮的 IPC 线程上）。失败无所谓 —— 那时退化成「等下次登录生效」。
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(PCWSTR::from_raw(section.as_ptr()).as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            100,
            None,
        );
    }
    let _ = HWND::default();
}

#[cfg(not(windows))]
fn broadcast_env_change() {}

// ==== IPC ====

/// 检测客户端环境变量冲突。
#[tauri::command]
pub async fn detect_env_conflicts(
    state: tauri::State<'_, crate::AppState>,
) -> Result<Vec<EnvFinding>, String> {
    let endpoint = crate::service::expected_endpoint(
        &state.store,
        &state.proxy,
        crate::model::CategoryType::ClaudeCli,
    );
    Ok(detect(&endpoint))
}

/// 移除用户勾选的那些（先备份）。
#[tauri::command]
pub async fn remove_env_conflicts(
    state: tauri::State<'_, crate::AppState>,
    names: Vec<String>,
) -> Result<RemovalResult, String> {
    let dir = crate::store::data_dir::app_data_dir()
        .map_err(|e| e.to_string())?
        .join("backups")
        .join("env-conflicts");
    let out = remove_at(&names, &dir).map_err(|e| e.to_string())?;
    if !out.removed.is_empty() {
        state.store.append_event(
            crate::model::CategoryType::ClaudeCli,
            "config",
            None,
            &format!(
                "已移除可能顶掉客户端配置的环境变量：{}（备份：{}）",
                out.removed.join("、"),
                out.backup_path
            ),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }
    fn names(f: &[EnvFinding]) -> Vec<String> {
        f.iter().map(|x| format!("{}:{}:{}", x.name, x.source, x.severity)).collect()
    }

    /// 🔴 **`OPENAI_*` 不许报成 `conflict`** —— 本模块与 CodexPlusPlus 最关键的一处分歧。
    ///
    /// 它的判据是裸 `name.starts_with("OPENAI_")` 全报成冲突。照抄的代价是：**每一个装过
    /// OpenAI SDK 的用户**都会看到一条「你的接入可能失效」，而我们有取证表明它通常无害
    /// （我们写的 provider 块不带 `env_key`，`codex.rs` 里有测试钉着）。
    ///
    /// 假警的代价在本仓是明确算过的 ——「指错方向的提示比没有提示更糟」：用户会去删一个
    /// 与故障无关的环境变量，然后带着「删了也没用」这个结论回来。
    #[test]
    fn openai_vars_are_a_notice_not_a_conflict() {
        let f = detect_from(&pairs(&[("OPENAI_API_KEY", "sk-x")]), &[], "http://127.0.0.1:47100");
        assert_eq!(names(&f), vec!["OPENAI_API_KEY:process:notice"]);
        assert!(
            f[0].note.contains("experimental_bearer_token") && f[0].note.contains("env_key"),
            "说明里必须给出「为什么通常无害」的取证，否则用户无从判断该不该管它：{}",
            f[0].note
        );
    }

    /// 判据是「**指向别处**」而不是「存在」。
    ///
    /// `ANTHROPIC_BASE_URL` 恰好等于我们的入口是完全正常的形态（用户自己配过、或上一次接入
    /// 留下的）。把它报成冲突等于叫用户去删一个**正确**的设置。
    #[test]
    fn a_base_url_that_already_points_at_us_is_not_a_conflict() {
        let ours = "http://127.0.0.1:47100";
        assert!(
            detect_from(&pairs(&[("ANTHROPIC_BASE_URL", ours)]), &[], ours).is_empty(),
            "指向我们自己的入口不是冲突"
        );
        // 尾部斜杠不该让它变成「指向别处」。
        assert!(detect_from(&pairs(&[("ANTHROPIC_BASE_URL", "http://127.0.0.1:47100/")]), &[], ours)
            .is_empty());
        let bad =
            detect_from(&pairs(&[("ANTHROPIC_BASE_URL", "https://api.anthropic.com")]), &[], ours);
        assert_eq!(names(&bad), vec!["ANTHROPIC_BASE_URL:process:conflict"]);
        assert!(
            bad[0].note.contains("api.anthropic.com") && bad[0].note.contains(ours),
            "两个值都要摆出来，用户才知道差在哪：{}",
            bad[0].note
        );
    }
    /// 🔴 凭据类的**值绝不外发**。
    ///
    /// 这些发现会进诊断报告（用户会发给别人）与界面。同 `lan_guard` 那条「明文令牌进事件
    /// 等于同时进了三个用户会分享出去的地方」。判据直接找那个值出现在任何字段里。
    #[test]
    fn credential_values_never_appear_anywhere_in_a_finding() {
        const SECRET: &str = "sk-ant-this-must-never-be-shown";
        let f = detect_from(
            &pairs(&[("ANTHROPIC_AUTH_TOKEN", SECRET), ("ANTHROPIC_API_KEY", SECRET)]),
            &[],
            "",
        );
        assert_eq!(f.len(), 2);
        for x in &f {
            let all = format!("{}{}{}", x.value, x.note, x.name);
            assert!(!all.contains(SECRET), "凭据明文进了发现里：{x:?}");
            assert!(x.has_value, "但必须报「设了」—— 否则用户不知道有这么一条");
            assert!(x.value.is_empty(), "凭据类的 value 必须是空串");
        }
        let sec = render_section(&f);
        assert!(!sec.contains(SECRET), "报告那一段也不许带值");
        assert!(sec.contains("已设置，值不外发"));
    }

    /// 空值不报：`set FOO=` 留下的空变量对客户端没有影响，报它是纯噪音。
    #[test]
    fn empty_values_are_not_reported() {
        assert!(detect_from(&pairs(&[("ANTHROPIC_API_KEY", "   ")]), &[], "").is_empty());
    }

    /// 前缀必须**在开头**匹配。`CUSTOM_ANTHROPIC_API_KEY` 是别人的变量，不是我们的事。
    #[test]
    fn the_prefix_must_be_at_the_start() {
        assert!(detect_from(&pairs(&[("CUSTOM_ANTHROPIC_API_KEY", "x")]), &[], "").is_empty());
        assert!(detect_from(&pairs(&[("MY_OPENAI_API_KEY", "x")]), &[], "").is_empty());
    }

    /// `ANTHROPIC_DEFAULT_*_MODEL` 按**前缀家族**认，不是枚举。
    ///
    /// 四档（opus/sonnet/haiku/fable）里 fable 是后来补的，而枚举式判据在补第五档时会静默漏掉
    /// —— 同「加一个字段要搜的是它的同族字段名」那条教训。
    #[test]
    fn the_tier_model_family_is_matched_by_shape_not_by_a_list() {
        for n in [
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_SOMETHINGNEW_MODEL",
        ] {
            let f = detect_from(&pairs(&[(n, "some-model")]), &[], "");
            assert_eq!(names(&f), vec![format!("{n}:process:conflict")], "{n} 应当被认出来");
        }
    }
    /// 🔴 目录变量两侧不一致 —— **CodexPlusPlus 刻意排除了 `CODEX_HOME`，而这是最静默的一种**。
    ///
    /// 三种形态都要报（详见 `directory_mismatches`），而两侧一致时必须**一条都不报**
    /// —— 设了 `CODEX_HOME` 本身完全正常。
    #[test]
    fn a_directory_variable_is_reported_only_when_the_two_sides_disagree() {
        let same = detect_from(
            &pairs(&[("CODEX_HOME", "D:\\cx")]),
            &pairs(&[("CODEX_HOME", "D:\\cx\\")]),
            "",
        );
        assert!(same.is_empty(), "两侧一致（且只差一个尾分隔符）不该报：{same:?}");

        let only_process = detect_from(&pairs(&[("CODEX_HOME", "D:\\cx")]), &[], "");
        assert_eq!(names(&only_process), vec!["CODEX_HOME:process:conflict"]);
        assert!(only_process[0].note.contains("双击启动"), "要说清用户双击启动时读的是别处");

        let only_user = detect_from(&[], &pairs(&[("CODEX_HOME", "D:\\cx")]), "");
        assert_eq!(names(&only_user), vec!["CODEX_HOME:user:conflict"]);
        assert!(only_user[0].note.contains("重启 SynaRoute"), "这一支的出路是重启我们自己");

        let both = detect_from(
            &pairs(&[("CODEX_SQLITE_HOME", "D:\\a")]),
            &pairs(&[("CODEX_SQLITE_HOME", "D:\\b")]),
            "",
        );
        assert_eq!(names(&both), vec!["CODEX_SQLITE_HOME:process:conflict"]);
        assert!(both[0].note.contains("D:\\a") && both[0].note.contains("D:\\b"));
    }

    /// 🔴 移除**只认我们报出来、且该用删除解决的那些** —— 否则这个 IPC 命令就是一个任意
    /// 注册表删除原语。
    ///
    /// 三种都必须被拒：没报过的（`PATH`）、报过但**不可移除**的（`CODEX_HOME` —— 那要对齐
    /// 两侧，删掉会破坏用户自己配的 Codex 布局）、空串。
    #[test]
    fn removal_refuses_names_we_never_reported_or_should_not_delete() {
        let dir = std::env::temp_dir().join(format!("sr-envrm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let found = detect_from(
            &pairs(&[("ANTHROPIC_API_KEY", "sk-x"), ("CODEX_HOME", "D:\\cx")]),
            &[],
            "",
        );
        let dirvar = found.iter().find(|f| f.name == "CODEX_HOME").expect("目录变量应当被报出来");
        assert!(!dirvar.removable, "目录变量不该带「可移除」这一位");

        let out = remove_from(
            &found,
            &["PATH".into(), "CODEX_HOME".into(), "".into()],
            &dir,
        )
        .unwrap();
        assert!(out.removed.is_empty(), "不该动任何一个不可移除/没报过的名字：{out:?}");
        assert!(out.backup_path.is_empty(), "什么都没做就不该留备份文件");
        assert!(!dir.exists(), "更不该为此建目录");
        assert!(out.note.contains("CODEX_HOME"), "要说清为什么目录类不给删：{}", out.note);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **备份失败一律不动手**（fail closed）。
    ///
    /// 同大脑聚合写文件那条：「备份没成还照删」等于无备份地改用户的系统设置。
    /// 夹具用「把备份目录那个路径做成一个文件」让 `create_dir_all` 失败 —— 跨平台都成立，
    /// 不依赖权限位（Unix 看目录权限、Windows 看文件属性，拿只读位当夹具会在另一个平台上失效）。
    ///
    /// ⚠️ **这个夹具只压得到两步中的第一步**（建目录），压不到第二步（写文件）——
    /// 备份文件名带时间戳，没法预先把它做成一个目录去让 `write` 失败，而只读属性在 Windows 上
    /// 拦不住目录内建文件。注入实测确认了这一点：把 `fs::write(..)?` 改成 `let _ = ..` 时
    /// 本条**仍绿**。故第二步由下面那条形态判据（`the_backup_write_must_propagate_its_error`）
    /// 机械钉住 —— 别把这条读成「两步都验过了」。
    #[test]
    fn nothing_is_removed_when_the_backup_cannot_be_written() {
        let base = std::env::temp_dir().join(format!("sr-envbk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let blocked = base.join("backups");
        std::fs::write(&blocked, b"I am a file, not a directory").unwrap();
        // 发现清单做入参 —— 不碰真实环境（套件并行跑，`set_var` 会串台）。
        let found = detect_from(&pairs(&[("ANTHROPIC_API_KEY", "sk-x")]), &[], "");
        assert!(found.iter().any(|f| f.removable), "夹具必须真的有一条可移除的，否则会早退");
        assert!(
            remove_from(&found, &["ANTHROPIC_API_KEY".into()], &blocked).is_err(),
            "备份写不了就必须整个失败，而不是继续删"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 备份那两步的错误都必须**往上抛**，不许 `let _ =` 吞掉。
    ///
    /// 上面那条夹具压不到 `fs::write` 那一步（理由见它的文档），而吞掉它的失效方向恰恰是
    /// 最坏的那个：**改了用户的系统环境变量、却没有任何可回退的记录**。
    /// 故这一步用形态判据钉住 —— 同 `mcp/stdio.rs` 那条「flush 的错误不许 `let _ =`」。
    #[test]
    fn the_backup_write_must_propagate_its_error() {
        let src = std::fs::read_to_string("src/tools/env_conflicts.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        // 只看 `remove_at` 那一段（往下到函数尾），免得把别处无害的 `let _ =` 算进来。
        let body = prod
            .split_once("fn remove_from(")
            .expect("函数改名了 —— 先修判据")
            .1;
        let body = &body[..body.find("\n}").unwrap_or(body.len())];
        assert!(
            body.contains("std::fs::create_dir_all(backup_dir)?"),
            "建目录那一步必须 `?` 抛出"
        );
        assert!(
            body.contains("std::fs::write(&path, serde_json::to_vec_pretty(&before)?)?"),
            "写备份那一步必须 `?` 抛出，且序列化失败也要抛（当前形态：{body:?}）"
        );
        assert!(
            !body.contains("let _ = std::fs::write"),
            "备份写入的错误被 `let _ =` 吞掉了 —— 那就是「删了但没有备份」"
        );
    }

    /// 一条都没有时报告里**不该多出这一段**（绝大多数用户的正常形态，不该多一行噪音）。
    #[test]
    fn a_clean_environment_adds_no_section_to_the_report() {
        assert!(render_section(&[]).is_empty());
    }

    /// 🔴 **源码级接线判据**：`detect` 必须真的去读两份环境，报告必须真的接上这一段。
    ///
    /// 上面全部判据都直调 `detect_from`（纯函数）—— 把 `detect` 里那两个入参换成空切片
    /// **它们照样全绿**，而那正是缺陷本体（功能整个不工作，且完全静默）。
    /// 这是本仓第 18 次盯同一类接线盲区。
    #[test]
    fn detect_must_read_both_environments_and_be_wired_into_the_report() {
        let src = std::fs::read_to_string("src/tools/env_conflicts.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(prod.contains("std::env::vars()"), "进程级那一份没读");
        assert!(prod.contains("&read_user_env()"), "用户级那一份没读");
        // 移除那一侧同理：白名单必须来自 `detect` 的产出，不许再独立判一次。
        assert!(
            prod.contains("remove_from(&detect(\"\"), names, backup_dir)"),
            "移除的白名单必须来自 detect 的产出 —— 两处各判一遍必然漂移（第一版就是那样，\
             于是界面上能点的 CODEX_HOME 被这里拒掉）"
        );
        let diag = crate::proxy::custom_headers::production_code_only(include_str!(
            "../diagnostics.rs"
        ));
        assert!(
            diag.contains("env_conflicts::diagnostics_section("),
            "报告里没接这一段 —— 那这份可观测性到不了排障者手上"
        );
    }

    /// 🔴 **「没能移除」不许报成「已移除」** —— 而这一条只能用源码级判据守。
    ///
    /// `remove_user_env_value` 在非 Windows 上返回 `Ok(false)`（那些平台没有「用户级环境
    /// 变量」这个单一可写来源）。第一版它返回 `Ok(())`，于是 Linux/macOS 上点「移除」
    /// 会报「已移除 N 个」而**一个字节都没持久化** —— 用户重启客户端后问题照旧，
    /// 而他已经相信这一步做完了。
    ///
    /// 为什么不是行为用例：`Ok(false)` 那一支在 Windows 上**编译进来但不可达**
    /// （本机 `cfg(windows)` 分支恒返回 `Ok(true)`），而 macOS CI 只跑 `cargo check`、
    /// 不跑测试。注入实测确认了这个盲区：把它改成 `removed.push(..)`，12 条用例照样全绿。
    /// 所以这里钉**形态**：三段代码必须同时在。
    #[test]
    fn a_removal_that_did_not_persist_must_not_be_reported_as_removed() {
        let src = std::fs::read_to_string("src/tools/env_conflicts.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("Ok(false) => failed.push("),
            "没能持久移除的必须进 failed，不许进 removed"
        );
        assert!(
            !prod.contains("Ok(false) => removed.push("),
            "反向：把「什么都没做」报成已移除，是本模块最坏的一种界面撒谎"
        );
        // 返回类型也要钉住 —— 退回 `Result<()>` 就没有「没能移除」这个信号了。
        assert!(
            prod.contains("fn remove_user_env_value(name: &str) -> std::io::Result<bool>")
                && prod.contains("fn remove_user_env_value(_name: &str) -> std::io::Result<bool>"),
            "两个 cfg 分支都必须返回 bool（成功与否 ≠ 有没有真的持久移除）"
        );
    }
}
