//! MCP **stdio 端**：客户端侧的身份标记 + stdio 子进程的转发。
//!
//! ## 为什么单独一个模块
//!
//! Codex 与 Claude 桌面端都不走 HTTP MCP，而是由客户端以子进程拉起
//! `synaroute.exe --mcp-stdio`，用 stdin/stdout 传 JSON-RPC。这条路与 [`crate::mcp`] 的
//! HTTP 服务端是两个方向：那边是「收请求」，这边是「当客户端 + 往主应用转发」。
//!
//! ## 🔴 分类身份在**注册时**写死，不问模型
//!
//! 历史缺陷：桌面端与 Codex 的注册形态**一字不差**（都是 `command=<exe>, args=["--mcp-stdio"]`），
//! 于是服务端没有任何信号能分辨它们，只能靠 `synaroute_ai` 的 `category` 参数当拐杖
//! —— 而模型不可能知道自己活在哪个客户端里，于是要么反问用户，要么省略后被默认成
//! `claude-cli`：用错 Key 池、额度记在别的分类头上、日志落错页，全是静默的。
//!
//! 修法是把分类写进注册本身（我们自己在写那份配置，那一刻分类是已知的）：
//!
//! ```text
//! 桌面端  args = ["--mcp-stdio", "--mcp-category=claude-desktop"]
//! Codex   args = ["--mcp-stdio", "--mcp-category=codex"]
//! ```
//!
//! 子进程读自己的 argv 拿到分类，再把它翻成**转发地址的路径段**
//! （`super::forward_url`）。刻意不改 JSON-RPC 载荷：HTTP 与 stdio 两条路
//! 于是共用同一套「路径段携带身份」的方案，服务端只有一处解析
//! （[`super::caller_from_path`]）。
//!
//! [`args`] 是这组 args 的**唯一事实来源**：`tools.rs` 的 JSON（桌面端）与
//! TOML（Codex）两个注册点都从它取，避免两处各拼一遍数组后分叉 —— 分叉的表现是
//! 「两端又变回无法分辨」，而那是静默的。

use crate::model::CategoryType;
use serde_json::{json, Value};

/// Codex / 桌面端 stdio MCP 的入口参数：`synaroute.exe --mcp-stdio`，
/// 进入无 UI 的 stdio JSON-RPC 模式（见 [`run_stdio`]）。
pub(crate) const MCP_STDIO_FLAG: &str = "--mcp-stdio";

/// 注册时写进 args 的分类标记前缀。**stdio 端唯一的身份来源**。
///
/// 用 `--flag=value` 单 token 形态而非 `--flag value` 两 token：客户端把 args 数组原样
/// 传给子进程，单 token 与顺序、与是否被别的参数插队都无关。
pub(crate) const CATEGORY_FLAG_PREFIX: &str = "--mcp-category=";

/// 某分类的 stdio 注册 args。
pub(crate) fn args(category: CategoryType) -> [String; 2] {
    [
        MCP_STDIO_FLAG.to_string(),
        format!("{CATEGORY_FLAG_PREFIX}{}", category.as_str()),
    ]
}

/// 桌面端 `claude_desktop_config.json` 用的 JSON 形态。
pub(crate) fn args_json(category: CategoryType) -> Value {
    Value::Array(args(category).into_iter().map(Value::String).collect())
}

/// Codex `config.toml` 用的 TOML 形态。
pub(crate) fn args_toml(category: CategoryType) -> toml::Value {
    toml::Value::Array(
        args(category)
            .into_iter()
            .map(toml::Value::String)
            .collect(),
    )
}

/// 从 argv 里取出分类标记。取不到（旧版注册尚未被重写）返回 None ——
/// 调用方须走 `_stdio` 哨兵段让服务端做精确兜底，**不能**自己默认成某个分类。
pub(crate) fn category_from_argv<I: IntoIterator<Item = String>>(
    args: I,
) -> Option<CategoryType> {
    args.into_iter()
        .filter_map(|a| a.strip_prefix(CATEGORY_FLAG_PREFIX).map(str::to_string))
        .find_map(|v| CategoryType::from_str(&v))
}

// ─── stdio 主循环 ───────────────────────────────────────────────────────────
//
// Codex 对 HTTP/streamable MCP 支持是实验性的（需 experimental_use_rmcp_client，且
// 握手挑剔、易「空壳」）。stdio 是 Codex 一等公民（codegraph/sqlcl 等均为 stdio），
// 稳定、无端口漂移、无首字节超时。桌面端更是**只认 stdio**。
//
// 复用与 HTTP 完全相同的 dispatch（initialize / tools/list / tools/call / ping），
// 只是传输层换成标准输入输出。通知（无 id）不回响应，与 HTTP 语义一致。

/// stdio MCP 主循环：逐行读 stdin 的 JSON-RPC 请求，处理后把响应逐行写 stdout。
/// 阻塞直到 stdin 关闭（客户端结束子进程时）。返回后进程应退出。
///
/// **关键设计（MSIX 宇宙错位的根治）**：本子进程**不读配置、不跑聚合**。
/// 早期版本让子进程自己 `dispatch`（含跑聚合），但 stdio 子进程被 Codex 桌面端
/// （MSIX 包）拉起时，继承其包身份 → 读 %APPDATA% 被虚拟化到包容器私有副本，
/// 那份配置没有用户在真实应用里配的 codex 聚合成员 → 永远「未启用大脑聚合」。
///
/// 现在改为**纯转发**：initialize/tools/list/ping 用本地静态响应（不依赖配置，
/// 且客户端启动即能握手拿到工具）；`tools/call` 转发到**运行中主应用**的 HTTP MCP
/// （127.0.0.1:{port}/mcp/…），主应用是用户双击启动的、读真实配置，聚合在那里跑。
/// localhost TCP 端口不受 MSIX 虚拟化影响（系统全局），故转发能跨包身份连通。
///
/// 分类身份来自**自己的 argv**（注册时写死，见 [`CATEGORY_FLAG_PREFIX`]）。在这里读一次、
/// 整个进程生命周期不变 —— 一个 stdio 子进程只属于一个客户端。
pub async fn run_stdio() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // 只读一次：本进程的身份是固定的。取不到 = 客户端配置是旧版（尚未被重写），
    // 此时转发到哨兵段，由服务端在「桌面端 / Codex」之间精确兜底并落一条可见事件。
    let caller = category_from_argv(std::env::args());

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF：客户端关闭了管道，退出循环 → 进程结束
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // 非法 JSON：忽略该行（无 id 无从回错）
        };

        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // 通知（无 id，如 notifications/initialized）：不回响应。
        if id.is_none() {
            continue;
        }
        let id = id.unwrap();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let resp = match super::local_static_response(method, &params) {
            // 握手/列举/心跳/资源探测：本地静态响应（与 HTTP dispatch 共用同一表），
            // 不依赖主应用是否已启动，保证客户端启动即能完成握手、看到 synaroute_ai。
            Some(Ok(result)) => super::rpc_ok(id, result),
            Some(Err((code, msg))) => super::rpc_error(id, code, &msg),
            // tools/call：转发到运行中主应用（持有真实配置）。
            None => match forward_tool_call_to_main(&params, caller).await {
                Ok(value) => super::rpc_ok(id, value),
                Err(msg) => super::rpc_ok(id, super::tool_error_content(&msg)),
            },
        };
        let mut out = serde_json::to_vec(&resp).unwrap_or_default();
        out.push(b'\n');
        if stdout.write_all(&out).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
}

/// 把 stdio 收到的 `tools/call` 转发到运行中主应用的 HTTP MCP。
/// 端口发现：先读平台共享端口文件（Windows=exe 同级；macOS=Application Support），
/// 读不到再扫描默认端口范围。主应用未运行时返回可读错误，提示用户启动应用。
///
/// 目标**路径带分类**（见 [`super::forward_url`]）：这是服务端唯一能知道
/// 「这次调用来自哪个客户端」的途径。
async fn forward_tool_call_to_main(
    params: &Value,
    caller: Option<CategoryType>,
) -> Result<Value, String> {
    let port = discover_main_mcp_port()
        .await
        .ok_or_else(|| "未找到运行中的 SynaRoute 主程序，请先启动 SynaRoute 桌面应用".to_string())?;
    let url = super::forward_url(port, caller);
    // 组装标准 MCP tools/call 请求（JSON-RPC over HTTP），转发给主应用。
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": params,
    });
    // 聚合可能耗时较久（多模型并行 + 决策者），给足超时（与 tool_timeout_sec 对齐留余量）。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("连接 SynaRoute 主程序失败({url}): {e}"))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析主程序响应失败: {e}"))?;
    // JSON-RPC 响应：优先取 result（工具结果），有 error 则透出其 message。
    if let Some(result) = v.get("result") {
        Ok(result.clone())
    } else if let Some(err) = v.get("error") {
        let m = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("主程序返回错误");
        Err(m.to_string())
    } else {
        Err("主程序响应缺少 result/error".into())
    }
}

/// 发现运行中主应用的 MCP 端口：先读平台共享端口文件，再扫描默认端口范围探活。
async fn discover_main_mcp_port() -> Option<u16> {
    // 1. 端口文件（主应用启动时写，最可靠）。读到后探活确认真的是 SynaRoute MCP。
    if let Some(p) = super::read_mcp_port_file() {
        if probe_mcp_alive(p).await {
            return Some(p);
        }
    }
    // 2. 兜底：扫描默认端口范围（端口文件缺失/过期时）。默认起始端口 9527，与 default_mcp_port
    //    对齐；主应用端口占用回退时也在 [9527, 9527+FALLBACK_RANGE] 内，故这里同范围扫描能覆盖。
    let start = 9527u16;
    for p in start..=start.saturating_add(super::FALLBACK_RANGE) {
        if probe_mcp_alive(p).await {
            return Some(p);
        }
    }
    None
}

/// 探活：向候选端口发一个最小 initialize，确认对面是活着的 SynaRoute MCP。
///
/// 刻意打**基址**（不带分类段）：initialize 是静态响应、与分类无关，
/// 且探活不该触发服务端「调用方未携带分类标识」那条告警（那条只在 tools/call 里判）。
async fn probe_mcp_alive(port: u16) -> bool {
    let url = super::base_url(port);
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": super::PROTOCOL_VERSION }
    });
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    match client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            // 确认是 SynaRoute（serverInfo.name），避免误连其它占用同端口的服务。
            if let Ok(v) = resp.json::<Value>().await {
                return v
                    .get("result")
                    .and_then(|r| r.get("serverInfo"))
                    .and_then(|s| s.get("name"))
                    .and_then(|n| n.as_str())
                    == Some("SynaRoute");
            }
            false
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 本轮的核心不变量：桌面端与 Codex 的 stdio args **必须不同**。
    ///
    /// 它们此前一字不差（都只有 `--mcp-stdio`），这正是「服务端分辨不出调用方、
    /// 只能问模型」的根因。一旦有人把分类标记去掉 / 写成同一个值，两端又变回无法分辨，
    /// 而失效是静默的（工具照样能调，只是永远走 claude-cli 的 Key 池）。
    #[test]
    fn desktop_and_codex_stdio_args_differ() {
        let d = args(CategoryType::ClaudeDesktop);
        let c = args(CategoryType::Codex);
        assert_ne!(d, c, "桌面端与 Codex 的 stdio args 不得相同（否则服务端无从分辨）");
        // 三端两两不同（CLI 虽走 HTTP，也不该与谁撞）。
        for a in CategoryType::ALL {
            for b in CategoryType::ALL {
                if a != b {
                    assert_ne!(args(a), args(b), "{a:?} 与 {b:?} 的 args 撞了");
                }
            }
        }
        // 形态锁定：第一个仍是入口 flag（lib.rs 靠它判 stdio 模式），第二个是分类标记。
        assert_eq!(d[0], MCP_STDIO_FLAG);
        assert_eq!(d[1], "--mcp-category=claude-desktop");
        assert_eq!(c[1], "--mcp-category=codex");
    }

    /// argv → 分类。写读两侧成对（`args` 写、`category_from_argv` 读），
    /// 用 round-trip 钉住：改了一侧忘了另一侧立刻红。
    #[test]
    fn category_from_argv_round_trips_every_category() {
        for c in CategoryType::ALL {
            let argv: Vec<String> = std::iter::once("synaroute.exe".to_string())
                .chain(args(c))
                .collect();
            assert_eq!(category_from_argv(argv), Some(c), "{c:?} 应能从 argv 读回");
        }
    }

    #[test]
    fn category_from_argv_is_position_independent() {
        // 客户端可能在 args 前后插别的参数；单 token `--flag=value` 形态与位置无关。
        let argv = [
            "synaroute.exe",
            "--mcp-category=codex",
            "--mcp-stdio",
            "--whatever",
        ]
        .map(String::from);
        assert_eq!(category_from_argv(argv), Some(CategoryType::Codex));
    }

    #[test]
    fn category_from_argv_absent_or_bogus_is_none() {
        // 旧版注册：只有入口 flag，没有分类标记 → None（调用方须走哨兵段，不得自己猜）。
        let legacy = ["synaroute.exe", "--mcp-stdio"].map(String::from);
        assert_eq!(category_from_argv(legacy), None, "旧配置必须判为「认不出」");

        // 认不出的值不能被当成某个分类（比如别名相近的拼错）。
        for bad in ["--mcp-category=", "--mcp-category=claude", "--mcp-category=codexx"] {
            let argv = ["synaroute.exe".to_string(), bad.to_string()];
            assert_eq!(category_from_argv(argv), None, "非法值应为 None: {bad}");
        }
    }

    /// JSON 与 TOML 两个注册点必须吐出同一组 token —— 它们是同一份 args 的两种序列化，
    /// 分叉的表现是「桌面端与 Codex 的行为不一致」，极难归因。
    #[test]
    fn stdio_args_json_and_toml_agree() {
        for c in CategoryType::ALL {
            let want = args(c);
            let j: Vec<String> = args_json(c)
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            let t: Vec<String> = args_toml(c)
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            assert_eq!(j, want.to_vec(), "JSON 形态与 args 不一致");
            assert_eq!(t, want.to_vec(), "TOML 形态与 args 不一致");
        }
    }
}
