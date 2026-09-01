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

/// stdio 子进程的诊断日志。**刻意不经 `Store`**。
///
/// # 为什么必须有它
///
/// 这一跳此前**零可观测性**：主应用记「我返回了结果」（`mcp` 事件带完整 trace），
/// Codex 记「我收到空」，而中间这个进程什么都不说。2026-08-29 那次排查就卡在这里 ——
/// 超时（Codex 300s / 转发 600s / 聚合 600s）、MCP 注册、`content` 为空三种假设
/// 逐一被排除后，剩下的候选全在本进程内，而现有日志一条都覆盖不到。
///
/// 子进程不碰 `Store` 是刻意的（见 [`run_stdio`]：被 MSIX 客户端拉起会继承包身份，
/// 读 `%APPDATA%` 被虚拟化），所以这里直接写 **exe 同级 `logs/`** ——
/// 与主应用那条「启动自检」用的是同一个已验证的非虚拟化通道。
///
/// 🔴 **macOS 必须另走一支**，理由与 [`super::mcp_port_file_path`] 一字不差：
/// `current_exe()` 在 `SynaRoute.app/Contents/MacOS/` 下，写进去会被 updater 的整包替换
/// 清掉、让 codesign 的 sealed resources 校验失败、在只读卷上直接写失败。
/// 而 macOS 压根没有 AppData 虚拟化，`~/Library/Application Support/` 对主应用与
/// stdio 子进程是同一份。第一版照搬了 exe 同级 —— **两个方向都静默**：mac 上日志进包内
/// 或写不出，Windows 上装在 Program Files 且同级不可写时一行都不写，
/// 而排障者按文档以为「这一跳已经有日志了」，去找一个永远不存在的文件。
///
/// # 三条刻意行为
///
/// - **单独一个文件**（不进 `YYYY-MM-DD.jsonl`）：那个文件由主应用的写线程独占、
///   并靠自己的体积记账做滚动分片（`log_rotate`），第二个追加者会把那份记账搞乱。
///   （不是「多进程写同一文件必然撕裂」—— 本文件自己就被多个 stdio 子进程共写，
///   每次 tool call 只 2~3 行短行，实践上够用。）
/// - **超过上限就整份重写**，不做滚动：`log_rotate::cleanup_old_logs_in` 按 `%Y-%m-%d`
///   解析文件名，这个名字解析不出来 → **永不被保留期清理**，不自己设上限就是无界增长。
/// - **绝不记 prompt / 响应正文**：只记 method、字节数、耗时、每一步的成败。
///   正文已经在主应用的 trace 里（那里有脱敏与体积上限），这里再写一份等于绕过它们
///   —— 而这个文件落在 exe 同级、不受保留期管、用户会直接贴出来（同 2026-08-27
///   那次令牌泄露的三个「用户会分享出去的地方」之一）。有判据盯着，见测试段。
fn diag(line: &str) {
    /// 1 MB 足够放下几千次调用的诊断行；到顶整份重写，不滚动（见函数文档）。
    const CAP: u64 = 1024 * 1024;
    let Some(dir) = diag_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("mcp-stdio.log");
    let over = std::fs::metadata(&path).map(|m| m.len() > CAP).unwrap_or(false);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let row = format!("{stamp} pid={} {line}\n", std::process::id());
    use std::io::Write as _;
    let opened = if over {
        std::fs::File::create(&path)
    } else {
        std::fs::OpenOptions::new().create(true).append(true).open(&path)
    };
    if let Ok(mut f) = opened {
        let _ = f.write_all(row.as_bytes());
    }
}

/// 诊断日志目录。平台分支与 [`super::mcp_port_file_path`] **必须保持一致** ——
/// 两者是同一个「stdio 子进程与主应用要读写同一份文件」的问题，分叉就等于其中一个走错地方。
pub(crate) fn diag_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(dirs::data_dir()?.join("SynaRoute").join("logs"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(std::env::current_exe().ok()?.parent()?.join("logs"))
    }
}

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
/// 这一行是不是一条**带 id 的 `ping`**？是则返回它的 id。
///
/// 只给「转发进行中插进来的行」用（见 [`run_stdio`] 里那个 `select!`）。刻意不在这里
/// 做完整解析与留痕：非 ping 的行会原样排队，由主循环按它自己那套流程处理并留痕，
/// 否则同一行会被记两遍 `recv`。
fn ping_id(line: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("method").and_then(Value::as_str) != Some("ping") {
        return None;
    }
    v.get("id").cloned().filter(|i| !i.is_null())
}

/// 这一行是不是**针对在途请求的取消通知**（`notifications/cancelled`）？
///
/// # 🔴 为什么必须认它（2026-09-01 用户实报）
///
/// 现场：Codex 停在审批门等用户点确认，467 秒时这次 `tools/call` 被取消
/// （Codex 自己记的是 `synaroute_ai result: aborted by user after 467.0s`），
/// 而我们**486.9 秒才把结果写出去** —— 那时已经没人在听了。用户看到的是
/// 「工具调用完成但返回内容为空……这通常是服务端超时或结果丢失」，**两个成因都不对**：
/// 没有超时（`tool_timeout_sec` 是 600s，我们 486s 就写完了），结果也没丢。
///
/// 不认它的代价是具体的：取消之后聚合还在跑，那 20 秒继续烧上游额度，而结果注定没人要；
/// 更糟的是排障方向被那句「服务端超时或结果丢失」带偏 —— 日志里 `forward ok` + `sent`
/// 一切正常，与「返回为空」完全对不上，而真相（对端早已放弃）在本进程里没有任何痕迹。
///
/// # 判据只认「取消的正是在途这一条」
///
/// `params.requestId` 必须等于当前正在转发的那个 id。MCP 允许客户端取消任意在途请求，
/// 而本进程同一时刻只跑一个 `tools/call`（其余排队，见 [`run_stdio`] 的 `select!`）——
/// 取消一个**排队中**的请求不该中止正在跑的这个。id 的类型可以是数字或字符串，
/// 故用 `Value` 全等比较而不是转成字符串（`1` 与 `"1"` 是不同的 JSON-RPC id）。
///
/// **不回响应**：它是 notification（无 `id`），按 JSON-RPC 规范回任何东西都是协议错误。
fn cancels_request(line: &str, in_flight: &Value) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return false;
    };
    if v.get("method").and_then(Value::as_str) != Some("notifications/cancelled") {
        return false;
    }
    v.get("params").and_then(|p| p.get("requestId")) == Some(in_flight)
}

/// 转发到主应用的 HTTP 超时，**派生自我们写给客户端的那个上限**。
///
/// # 🔴 这两个 600 不是巧合 —— 其中一个就是我们自己写的
///
/// 本注释上一版写着「不是客户端的 `tool_timeout_sec`（那个我们不写、也读不到）」
/// 与「本仓从不写它（全仓零写入点，**已核对**）」。**两句都是假话**：
/// [`crate::tools`] 里那个常量正是写进 `~/.codex/config.toml` 的
/// `tool_timeout_sec` 的值（`entry.insert("tool_timeout_sec", …)`），而且它还进了
/// 幂等判据 —— 用户手改小了，下一次接入会被我们改回来。所以客户端的容忍上限
/// 不是「读不到」，是**由我们决定**的。
///
/// 那句「已核对」让它比普通过时注释更贵：它声称做过取证，于是下一个人不会再查。
///
/// # 为什么必须派生而不是各写一个 600
///
/// 语义上这两个数**应当相等**：我们等主应用的时间，就该正好等于我们让客户端等我们的时间。
/// 各写一份的漂移方向有一条是**静默**的 ——
///
/// - 谁把 `MCP_TOOL_TIMEOUT_SEC` 调大到 900：`slow_note` 从 420s 起就开始告警，
///   而客户端还能再等 480s → 纯噪音（响亮、可发现）。
/// - 谁把它调**小**到 300：客户端 300s 就放弃，而 `slow_note` 要到 420s 才出声
///   —— 也就是说**在它该报警的那些次里一个字都不说**，而那正是本函数存在的全部理由。
///
/// 派生之后这条漂移在编译期就不可能发生。
const FORWARD_TIMEOUT_SECS: u64 = crate::tools::MCP_TOOL_TIMEOUT_SEC as u64;

// `as u64` 在负值下会绕成天文数字（那会让本进程实际上永不超时）。负的工具超时本身是荒谬的
// —— 它同时会写进 TOML 给客户端 —— 故在编译期挡掉，而不是留一条运行期分支。
const _: () = assert!(
    crate::tools::MCP_TOOL_TIMEOUT_SEC > 0,
    "MCP_TOOL_TIMEOUT_SEC 必须为正：它既是我们写给客户端的上限，也是本进程 HTTP 超时的来源"
);

/// 一次转发耗时逼近客户端超时上限时的告警后缀（够快则为空串）。
///
/// # 为什么这条值得单独存在
///
/// 客户端的上限就是 [`FORWARD_TIMEOUT_SECS`]（同一个数，见它的文档 —— 那个值由我们
/// 写进 `~/.codex/config.toml`），所以这里的百分比是**对着客户端真实容忍度**算的，
/// 不是拿一个不相干的本地数字当参照。
///
/// 2026-09-01 的现场是 486.9 秒（上限 600s，余量只剩 19%），而三份日志里没有任何东西
/// 提示「这次差一点就超时了」。真正超时之后症状与被取消一模一样（客户端显示空），
/// 而那时已经来不及取证 —— 告警必须在**还没超时**的那些次就开始出现。
///
/// ⚠️ 唯一仍看不到的情形：用户手改了 config 里那个数字，而我们**还没重新接入**
/// （重新接入会按幂等判据把它改回 `MCP_TOOL_TIMEOUT_SEC`）。那段窗口里参照物偏大。
///
/// 阈值取 70%：低了会在正常的多模型聚合上刷噪音（成员 + 决策者跑上几分钟是常态），
/// 高了则留不出「下次调小成员数或超时」的反应余地。
fn slow_note(ms: u128) -> String {
    let cap = u128::from(FORWARD_TIMEOUT_SECS) * 1000;
    if ms * 100 < cap * 70 {
        return String::new();
    }
    format!(" ⚠ 已用 {}% 的超时预算（上限 {FORWARD_TIMEOUT_SECS}s）", ms * 100 / cap)
}

/// 写一条响应给客户端。返回 `false` = 管道已不可用，调用方该收尾退出。
///
/// 抽成函数是因为**两处**要写：主循环，以及转发进行中即时回的那条 `ping`。
async fn write_resp(stdout: &mut tokio::io::Stdout, resp: &Value, method: &str) -> bool {
    use tokio::io::AsyncWriteExt;
    let mut out = serde_json::to_vec(resp).unwrap_or_default();
    out.push(b'\n');
    let bytes = out.len();
    if let Err(e) = stdout.write_all(&out).await {
        diag(&format!("write_all FAILED method={method} bytes={bytes}: {e}"));
        return false;
    }
    // 🔴 `flush` 的错误此前是 `let _ =` 吞掉的。`write_all` 只保证进了缓冲区，
    // **真正让对方看见的是 flush** —— 吞掉它的失效形态正是「我们以为发出去了、
    // 对方什么也没收到」，也就是 2026-08-29 那次「三次调用都返回空」的候选根因之一。
    if let Err(e) = stdout.flush().await {
        diag(&format!("flush FAILED method={method} bytes={bytes}: {e}"));
        return false;
    }
    if method == "tools/call" {
        diag(&format!("sent method={method} bytes={bytes}"));
    }
    true
}

pub async fn run_stdio() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // 只读一次：本进程的身份是固定的。取不到 = 客户端配置是旧版（尚未被重写），
    // 此时转发到哨兵段，由服务端在「桌面端 / Codex」之间精确兜底并落一条可见事件。
    let caller = category_from_argv(std::env::args());

    // 🔴 进程启动就留一行。没有它，「日志文件是空的」在四种成因间完全无法区分：
    // 子进程压根没被客户端拉起 / 拉起了但客户端一个字节都没发 / logs 目录不可写 /
    // 握手那行 JSON 没解析成。而「文件里什么都没有」正是 2026-08-29 那次卡住的形态。
    // 同时记下认出的分类 —— 这一跳唯一有历史的静默失效维度就是「认成了 claude-cli」。
    diag(&format!("start caller={caller:?}"));

    // 🔴 stdin 交给一个独立任务读，主循环只从 channel 取整行。
    //
    // 为什么不能在主循环里直接 `select!` 一个 `read_line`：`AsyncBufReadExt::read_line`
    // **不是 cancel-safe** 的 —— 转发先完成时那个 future 被 drop，已经读进缓冲的半行就丢了，
    // 而丢半行等于把 JSON-RPC 流撕开（此后每一行都解析失败，表现是「工具突然全坏」）。
    // `mpsc::Receiver::recv` 才是 cancel-safe 的，故把「读」隔离进任务、只在 channel 上 select。
    //
    // 容量 64 是背压：客户端灌得太快时读任务自己阻塞在 send 上，不会把行无界堆进内存。
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin());
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) | Err(_) => break, // EOF / 读错误：客户端关了管道
                Ok(_) => {}
            }
            if tx.send(buf.clone()).await.is_err() {
                break; // 主循环已退出，没人收了
            }
        }
    });

    let mut stdout = tokio::io::stdout();
    // 转发进行中到达、且**不该抢先回答**的请求：排队等本次调用结束（见下面 select 的注释）。
    let mut pending: std::collections::VecDeque<String> = std::collections::VecDeque::new();

    loop {
        // 先消化排队的，再去 channel 取新的 —— 否则「转发期间攒下的请求」会被后到的插队。
        let line = match pending.pop_front() {
            Some(l) => l,
            None => match rx.recv().await {
                Some(l) => l,
                None => break, // stdin 已关且队列已空 → 进程该退出
            },
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            // 非法 JSON：忽略该行（无 id 无从回错）。**只记长度不记内容** —— 这条 continue
            // 此前完全无痕，而「握手那行没解析成」就藏在这里。
            Err(_) => {
                diag(&format!("bad json, {} bytes ignored", trimmed.len()));
                continue;
            }
        };

        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // 通知（无 id，如 notifications/initialized）：不回响应。
        if id.is_none() {
            continue;
        }
        let id = id.unwrap();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // 收到即记，与下面的 `sent` 配成一对。两者的时间差把「一次 tools/call 有多长」
        // 以及「期间排了几条请求」暴露出来。
        diag(&format!("recv method={method}"));

        let resp = match super::local_static_response(method, &params) {
            // 握手/列举/心跳/资源探测：本地静态响应（与 HTTP dispatch 共用同一表），
            // 不依赖主应用是否已启动，保证客户端启动即能完成握手、看到 synaroute_ai。
            Some(Ok(result)) => super::rpc_ok(id, result),
            Some(Err((code, msg))) => super::rpc_error(id, code, &msg),
            // tools/call：转发到运行中主应用（持有真实配置）。
            None => {
                let t0 = std::time::Instant::now();
                let fwd = forward_tool_call_to_main(&params, caller);
                tokio::pin!(fwd);
                // 🔴 转发期间**继续读 stdin**，但只即时回 `ping`。
                //
                // 主循环此前在这里一动不动最长 600s：客户端的 keepalive ping 排在队里得不到
                // 回应，于是它认为 MCP server 已死 → 断开或杀掉子进程，用户看到的正是
                // 「工具不可用 / 返回空」，而那恰恰是本轮要归因的症状。
                //
                // **刻意只放 ping 过去**，其余请求一律排队：让两个 `tools/call` 并发跑是
                // 另一件事（两轮聚合同时烧额度、响应还会乱序，而客户端未必都容得下乱序），
                // 而 `ping` 是纯本地静态回答、零副作用。范围最小、收益最大。
                let mut stdin_open = true;
                // `None` = 正常跑完；`Some(())` = 被对端取消，见 `cancels_request`。
                let mut cancelled = None;
                let r = loop {
                    tokio::select! {
                        res = &mut fwd => break Some(res),
                        got = rx.recv(), if stdin_open => match got {
                            // stdin 关了也要把这次转发跑完并写出去（写失败会在下面收尾）。
                            // 必须置 false：否则 recv 立刻再返 None，这里就成了忙等。
                            None => stdin_open = false,
                            Some(l) if cancels_request(&l, &id) => {
                                // 🔴 取消即**放弃这次转发**（`fwd` 在这里被 drop → HTTP 连接断开），
                                // 且**不写响应** —— 对端已经不认这个 id 了，写过去只会被丢掉，
                                // 而在它看来这次调用「返回了空」。
                                cancelled = Some(());
                                break None;
                            }
                            Some(l) => match ping_id(&l) {
                                Some(pid) => {
                                    diag("recv method=ping (转发进行中，即时回)");
                                    if !write_resp(&mut stdout, &super::rpc_ok(pid, json!({})), "ping").await {
                                        stdin_open = false;
                                    }
                                }
                                None => pending.push_back(l),
                            },
                        },
                    }
                };
                let ms = t0.elapsed().as_millis();
                // 被取消：留一行**成因明确**的痕迹后回到主循环。这一行是本条修复的全部价值 ——
                // 没有它，「对端早已放弃」在三份日志里都看不出来（主应用记成功、Codex 记空）。
                if cancelled.is_some() {
                    diag(&format!("forward cancelled by peer method={method} {ms}ms（未写响应）"));
                    continue;
                }
                // `break None` 只在取消那一支发生，已在上面 continue 掉。
                let Some(r) = r else { continue };
                match r {
                    Ok(value) => {
                        // 刻意**不**在这里记结果长度：那要把整个结果再序列化一遍，而失败时
                        // 会显示成 `0` —— 与「返回空」这个正在查的症状撞车。长度统一由
                        // `sent` 那行的 `bytes` 给（它是真正写出去的字节数）。
                        diag(&format!("forward ok method={method} {ms}ms{}", slow_note(ms)));
                        super::rpc_ok(id, value)
                    }
                    Err(msg) => {
                        // 转发失败会变成一段可读的工具错误文本发回客户端（不是空）——
                        // 记一行是为了让「客户端显示了错误」与「客户端显示了空」在日志里可分辨。
                        diag(&format!("forward err method={method} {ms}ms: {msg}"));
                        super::rpc_ok(id, super::tool_error_content(&msg))
                    }
                }
            }
        };
        // 写失败即退出循环：管道已经不可用，继续读下一条请求只会攒出更多无人收的响应。
        if !write_resp(&mut stdout, &resp, method).await {
            break;
        }
    }
    diag("stdio loop ended (stdin EOF or write error)");
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
    // 聚合可能耗时较久（多模型并行 + 决策者），给足超时。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FORWARD_TIMEOUT_SECS))
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

    /// 🔴 `flush` 的错误**不许被吞**，而且这一跳必须有可观测性。
    ///
    /// 背景：2026-08-29 那次「三次调用 `synaroute_ai` 都返回空」查到最后卡在本进程 ——
    /// 超时（Codex 300s / 转发 600s / 聚合 600s）、MCP 注册、`content` 为空三种假设
    /// 全部排除后，剩下的候选都在这里，而当时**一条日志都没有**。
    ///
    /// `write_all` 只保证进缓冲区，真正让对方看见的是 `flush`；原实现
    /// `let _ = stdout.flush().await;` 把它的错误丢了 —— 失效形态正是
    /// 「我们以为发出去了、对方什么也没收到」。
    #[test]
    fn the_flush_error_must_not_be_swallowed_and_the_hop_must_be_observable() {
        let src = std::fs::read_to_string("src/mcp/stdio.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            !prod.contains("let _ = stdout.flush()"),
            "flush 的错误不许用 `let _ =` 吞掉 —— 那是「以为发出去了、对方没收到」的成因"
        );
        assert!(
            prod.contains("stdout.flush().await") && prod.contains("flush FAILED"),
            "flush 必须处理错误并留痕"
        );
        assert!(
            prod.contains("write_all FAILED"),
            "write_all 失败也要留痕，否则与 flush 失败在日志里无从分辨"
        );
        assert!(
            prod.contains("forward ok") && prod.contains("forward err"),
            "转发的成败与耗时必须记 —— 主应用侧只知道自己返回了，分辨不出客户端有没有收到"
        );
    }

    /// 🔴 诊断日志**绝不带正文**，而且判据必须按**整个调用表达式**判、不能按行。
    ///
    /// 正文（prompt / 聚合结果）已经在主应用的 trace 里，那里有脱敏与体积上限；
    /// 在这里再写一份等于绕过两者，而这个文件还落在 exe 同级、不受保留期管、
    /// 用户会直接贴出来（同 2026-08-27 那次令牌泄露的三个「用户会分享出去的地方」之一）。
    ///
    /// ⚠️ **第一版按行扫，而文件里唯一的多行 `diag` 调用整个逃出了扫描范围**（实测注入：
    /// 在那个多行调用的续行里塞一个 `prompt=…` 照样全绿）。同本仓「判据存在 ≠ 判对了维度」
    /// 那条。现在按「从 `diag(` 扫到分号」取整段。
    ///
    /// 另加两条：`println!`/`print!` 一律禁 —— **stdout 是 JSON-RPC 协议信道**，
    /// 往里写一个字节就会让客户端的 MCP「握不上手 / 工具是空壳」，而这类症状最难归因到
    /// 一句调试打印上；而且本模式下 tracing **没有 subscriber**（`lib.rs` 里 stdio 早退
    /// 排在 tracing init 之前），`warn!` 是空操作 —— 想在这里排障只能用 `diag`。
    #[test]
    fn the_diag_log_never_carries_prompt_or_response_text() {
        let src = std::fs::read_to_string("src/mcp/stdio.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        let mut calls = 0usize;
        let mut rest = prod.as_str();
        while let Some(at) = rest.find("diag(") {
            rest = &rest[at + 5..];
            let end = rest.find(';').unwrap_or(rest.len());
            let expr = &rest[..end];
            calls += 1;
            for banned in ["prompt", "arguments", "text", "analysis", "params", "content"] {
                assert!(
                    !expr.contains(banned),
                    "诊断调用不许携带 `{banned}`（正文只该进主应用的 trace）：{}",
                    expr.trim()
                );
            }
        }
        // 解析到太少就主动判失败：调用形态一变（比如包一层 helper），
        // 上面那个循环会静默退化成「什么都没查」。同 `invoke-command-must-exist` 那条门。
        assert!(calls >= 6, "只扫到 {calls} 处 diag 调用，判据多半已失效");
        for banned in ["println!", "print!"] {
            assert!(
                !prod.contains(banned),
                "stdout 是 JSON-RPC 协议信道，`{banned}` 会直接污染它（本模式下也没有 tracing subscriber，排障只能用 diag）"
            );
        }
        // 上限必须在：这个文件名解析不出日期，`cleanup_old_logs_in` 永不清理它。
        assert!(
            prod.contains("const CAP:") && prod.contains("File::create"),
            "必须有体积上限并在到顶时整份重写，否则是无界增长"
        );
        // macOS 必须另走一支：exe 同级在 .app bundle 内部（updater 整包替换会清掉、
        // codesign 校验会失败、只读卷直接写失败）。平台分支与 mcp_port_file_path 同源。
        assert!(
            prod.contains("target_os = \"macos\"") && prod.contains("dirs::data_dir()"),
            "diag_dir 必须有 macOS 分支，否则 mac 上日志写进 app 包内部或压根写不出"
        );
        // 🔴 接线判据：可观测性存在于磁盘上但到不了排障者手上，等于没有。
        // 用户交出来的是诊断报告和 `logs/*.jsonl` —— 这个文件必须在报告里点名，
        // 否则没人知道去 tail 它（也没有任何界面/文档提到它）。删掉那行不会有别的东西变红。
        let diagnostics = std::fs::read_to_string("src/diagnostics.rs").unwrap();
        let dprod = crate::proxy::custom_headers::production_code_only(&diagnostics);
        assert!(
            dprod.contains("mcp-stdio.log") && dprod.contains("stdio::diag_dir()"),
            "诊断报告必须打出 mcp-stdio.log 的路径 —— 且要用 diag_dir()，\
             不能拿 effective_log_dir 拼（用户改过 logDir 时那是另一个目录）"
        );
    }

    /// 🔴 转发进行中只有 `ping` 能插队，而且 stdin 必须由独立任务读。
    ///
    /// 两条不变量，各对应一个真实故障：
    ///
    /// ① **`read_line` 不许出现在 `select!` 的分支里** —— 它**不是 cancel-safe** 的：
    ///    转发先完成时那个 future 被 drop，已读进缓冲的半行就丢了，而丢半行等于把
    ///    JSON-RPC 流撕开（此后每一行都解析失败，表现是「工具突然全坏」）。
    ///    故生产段里 `read_line` 只该出现**一次**：在那个专职读 stdin 的任务里。
    ///
    /// ② **非 ping 的请求必须排队，不许即时回答**：让两个 `tools/call` 并发跑是另一件事
    ///    （两轮聚合同时烧额度、响应还会乱序，而客户端未必都容得下乱序）。
    #[test]
    fn only_ping_may_jump_the_queue_and_stdin_must_be_read_by_a_task() {
        let src = std::fs::read_to_string("src/mcp/stdio.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("read_line(").count(),
            1,
            "read_line 不是 cancel-safe 的，只许在专职读 stdin 的任务里出现一次 —— \
             放进 select! 分支会在转发先完成时丢掉半行，把 JSON-RPC 流撕开"
        );
        // 更直接的一刀：那唯一一次必须排在 `select!` **之前**（= 在读任务里，
        // 而不是在 select 的某条分支里）。只查次数的话，「把读任务删掉、改在分支里读」
        // 仍然是 1 次、判据会绿。
        let last_read = prod.rfind("read_line(").expect("上面刚断言过存在");
        let select_at = prod.find("tokio::select!").expect("找不到 select! —— 判据失去参照物");
        assert!(
            last_read < select_at,
            "read_line 只能出现在 select! 之前的读任务里，不许出现在 select 分支中"
        );
        assert!(
            prod.contains("tokio::select!") && prod.contains("rx.recv(), if stdin_open"),
            "转发期间必须继续从 channel 取行（否则客户端 keepalive ping 排队超时，\
             它会认为 MCP server 已死并杀掉子进程）"
        );
        assert!(
            prod.contains("pending.push_back(l)"),
            "非 ping 的请求必须排队等本次 tools/call 结束，不许并发抢答"
        );
        // `if stdin_open` 那个门是防忙等的：recv 返回 None 后不置 false，select 会立刻
        // 再次拿到 None，这个 loop 就变成 100% CPU 空转。
        assert!(
            prod.contains("None => stdin_open = false"),
            "stdin 关闭后必须停掉那条 select 分支，否则是忙等"
        );
    }

    /// `ping_id` 只认「带 id 的 ping」。判错的两个方向都有代价：
    /// 认漏了 → keepalive 仍然排队（回到缺陷本身）；认多了 → 别的请求被回成空对象。
    #[test]
    fn ping_id_recognizes_exactly_the_pings_that_need_an_answer() {
        assert_eq!(ping_id(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#), Some(json!(7)));
        assert_eq!(
            ping_id(r#"{"jsonrpc":"2.0","id":"abc","method":"ping","params":{}}"#),
            Some(json!("abc"))
        );
        // 通知形态的 ping（无 id）不需要响应 —— 回一条反而是协议错误
        assert_eq!(ping_id(r#"{"jsonrpc":"2.0","method":"ping"}"#), None);
        assert_eq!(ping_id(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#), None);
        // 别的方法一律排队，不许在这里被当成 ping 回掉
        assert_eq!(ping_id(r#"{"id":1,"method":"tools/call"}"#), None);
        assert_eq!(ping_id(r#"{"id":1,"method":"tools/list"}"#), None);
        assert_eq!(ping_id("not json"), None);
        assert_eq!(ping_id(""), None);
    }

    /// 🔴 `FORWARD_TIMEOUT_SECS` 必须**派生**自我们写给客户端的那个上限，不许各写一个 600。
    ///
    /// 值相等这一半由类型系统保证（它就是那个常量），故本判据钉的是**来源**——
    /// 同 `the_two_sse_invariants_must_stay_derived_not_assumed` 的思路：钉来源，不钉结论。
    /// 谁把它改回字面量 `600`，两个数就能各自漂移，而其中一个方向是**静默**的：
    /// `MCP_TOOL_TIMEOUT_SEC` 调小到 300 时客户端 300s 就放弃，而 `slow_note` 要到 420s
    /// 才出声 —— 在它唯一该说话的那些次里一个字都不说。
    ///
    /// 上一版注释还声称「那个我们不写、也读不到」「全仓零写入点，**已核对**」，
    /// 而 `tools.rs` 里写它的那行一直都在。带「已核对」字样的假话比普通过时注释更贵
    /// —— 它声称做过取证，于是下一个人不会再查。这条判据同时钉死那句话不会复活。
    #[test]
    fn the_forward_timeout_must_stay_derived_from_what_we_tell_the_client() {
        assert_eq!(
            FORWARD_TIMEOUT_SECS,
            crate::tools::MCP_TOOL_TIMEOUT_SEC as u64,
            "我们等主应用的时间，必须正好等于我们让客户端等我们的时间"
        );
        let src = crate::proxy::custom_headers::production_code_only(include_str!("stdio.rs"));
        let line = src
            .lines()
            .find(|l| l.contains("const FORWARD_TIMEOUT_SECS"))
            .expect("常量还在吗");
        assert!(
            line.contains("MCP_TOOL_TIMEOUT_SEC"),
            "必须派生，不许写死字面量（两处各写一个 600 必然漂移）：{line}"
        );
        // 那两句假话不许回来。
        assert!(
            !src.contains("我们不写、也读不到") && !src.contains("全仓零写入点"),
            "`tool_timeout_sec` 由 tools.rs 写入，别再声称我们不写它"
        );
    }

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
