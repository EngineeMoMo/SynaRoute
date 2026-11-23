# 代码审查修复记录（2026-11-23）

本轮修复基于 SynaRoute 0.1.96、提交 `878866e972653b309fde676dca1571f724597a41`。原审查确认的六个缺陷均已修复，并补入正式回归测试；另完成 CSP、Host 和 codegraph 加固，以及可兼容更新的依赖漏洞修复。本轮改动纳入 0.1.97 发布准备；本文记录提交前的修复与验证，实际发布状态以 Git tag 和 GitHub Release 为准。

后续复核发现的三项边界缺陷，以及 CSP 自动验收和 codegraph 进程树清理，已在同日继续修复；实现与最终验证见 [后续修复记录](security-followup-fixes-2026-11-23.md)。本文件下方的测试数量保留为第一轮结果。

审查日期按 America/New_York 记录；验证环境为 Windows、Rust 1.99.0、Node.js 22.9.0。原报告和六个失败探针保留在 `output/audit-2026-11-23/`，生产代码没有引用该目录。

## 1. 六个缺陷的修复

| 编号 | 修复后的行为 | 正式回归证据 |
| --- | --- | --- |
| F1：SSE 错误泄漏凭据 | 完整事件解析后才转发，错误中的实际 API Key、自定义请求头值和 JSON 字段名均脱敏；支持任意 chunk 拆分和 JSON 转义。HTTP 错误及 200 包装错误复用相同处理。正常内容保持原字节，畸形、超限、残缺错误帧返回通用错误 | `proxy/stream_redaction.rs`：三协议、六转换方向、逐块边界、转义、HTTP/软错误、EOF/超限；`proxy/active_requests_tests.rs`：真实 HTTP 上游及日志断言 |
| F2：工具参数丢字段名 | 保留字段名及字符串、数字、布尔值，嵌套对象也纳入检测，例如 `password=...` | `routing/privacy.rs`：三协议、嵌套参数、数字密码 |
| F3：消息拼接破坏检测边界 | 消息和独立字段之间保留分隔，同时检查相邻文本块的拼接视图；合并结果不重复计数，不改写原请求 | `routing/privacy.rs`：system 前缀、相邻块前缀、跨块凭据 |
| F4：重启端口漂移 | 下一次启动先等待旧监听任务退出；并发启动串行化，取消等待不丢失退出状态，停止操作可撤销在途启动。设置更新与启动互斥；确需换端口时同步客户端配置，失败停止对应代理并返回错误 | `proxy/active_requests_tests.rs`：反复启停、并发启动、LAN 切换、取消启动、设置锁；现有服务层配置测试 |
| F5：加密思考历史误选模型 | 将 `redacted_thinking` 识别为不透明历史；同一已观察会话保留原模型，不再发送判断请求。不能确认原模型时拒绝 | `routing/identity.rs`、`routing/matrix_tests.rs`：已观察历史沿用、陌生或缺失身份拒绝、判断次数 |
| F6：Responses 合法续传被拒 | 会话身份不依赖增量请求的首条消息。只信任代理实际观察到的原生 Responses 终态 ID，将引用绑定至会话、分支、来源、配置、Key 和真实模型；工具输出续传也可识别 | `routing/matrix_tests.rs`、`routing/continuation_http_tests.rs`：JSON/SSE 真实 HTTP 两次续传、未知 ID 422、隔离、候选限制、失败清理和 reset |

### 相关边界

- SSE 单帧最多 8 MiB。无法检查的非 SSE 或仍带压缩编码的流式响应会明确拒绝；不能以未检查的字节绕过错误脱敏。
- Responses 引用登记最多 4096 条，24 小时过期；只记录原生 Responses 响应，协议转换合成的 ID 不充当服务端引用。重启、清空记录、配置变化、旧回合或原线路不可用时，续传可能被拒绝，不会把引用交给其他供应商凭据。
- 先前审查的 F6 探针仅调用 `served()` 并自行构造响应 ID。修复后的行为要求真实观察响应 ID，该探针由正式 HTTP 测试替代，不能把自行构造的未知 ID 当成应成功的续传。
- 隐私规则仍有检测范围限制，图片及加密内容不可扫描，具体使用说明见 [路由策略手册](37-路由策略使用手册.md)。

## 2. 安全加固

| 项目 | 实施内容 | 验证 |
| --- | --- | --- |
| WebView CSP | 配置生产 CSP，限制脚本、外联、iframe、object 和表单；保留本地资源、Tauri IPC、内联样式、数据图片和已使用的头像来源；开发配置包含 Vite 本地连接 | 实际 Windows WebView2 加载构建资源：资源加载、React 根节点、数据图片正常；内联脚本、data 脚本、eval 和外部 fetch 被阻止；最小 IPC 回传成功 |
| HTTP Host | 主代理及 MCP 同时校验 Host 与既有 Origin；Host 必须匹配实际连接的本地 IP/端口，localhost 仅用于回环；拒绝任意 DNS 域名、重复 Host 和请求 URI authority 不匹配 | IPv4、IPv6、LAN、恶意域名、端口、重复 Host；原有 keepalive/入口测试和 worker 共享测试 |
| codegraph | stdout/stderr 各限 4 MiB；同一超时覆盖子进程退出及两路输出读取；通过 Windows Job Object / Unix 进程组清理后代进程，覆盖超时、取消及父进程正常退出。JSON 解析错误只报告行列，避免记录原始输出；符号查询及影响面元数据使用文件读取路径策略过滤 | 真实子进程输出超限、继承管道关闭、后代进程心跳停止；假 CLI 穿过实际 query/impact/callers/callees 路径，过滤敏感文件和越界路径；错误日志不含原始输出 |

LAN 客户端需使用实际 IP 地址；任意 DNS 别名不在 Host 白名单中。codegraph 的资源限制不是第三方程序的完整沙箱，未承诺收容或终止主动脱离的任意后代进程。

## 3. 依赖处理与剩余维护公告

- 主应用和 Laya worker：`url 2.5.8 → 2.5.9`，`form_urlencoded 1.2.2 → 1.2.3`，处理 `RUSTSEC-2026-0592`；两个 Cargo 清单同时提高最低版本。
- 主应用：`cookie 0.18.1 → 0.18.3`，处理复审新增的 `RUSTSEC-2026-0594`。公告的触发条件涉及 Unix fork 后私有 cookie nonce；没有将其描述为已在本应用复现的攻击链。
- 直接时间依赖 `chrono` 替换为 `jiff 1.0.2`。时间戳毫秒、RFC3339 的 `+00:00` 及 0/3/6/9 位小数精度保持兼容；用量仍按本地日历日入账，会话分组按本地日期差计算。回归覆盖日期边界、夏令时、跨日延迟落盘及历史时间戳。

最终 OSV registry 扫描：主锁文件 625 个包、worker 锁文件 217 个包；漏洞类公告已无命中。主应用 npm 与官网 npm 各为 0 条报告漏洞。扫描仍报告以下停止维护类公告，**未将其隐藏或宣称全部清零**：

| 依赖 | 公告 | 当前状态 |
| --- | --- | --- |
| chrono 0.4.45 | RUSTSEC-2026-0543 | 主代码和直接依赖已移除；仍由 serde_with 的可选依赖进入锁文件。Windows x64、Linux x64、macOS arm64 的当前默认功能依赖树均无激活引用 |
| proc-macro-error 1.0.4 | RUSTSEC-2024-0370 | GTK3/GLib 宏依赖链残留；保留已有 GLib 安全回补固定提交 |
| zbus_names 4.3.3、zvariant 5.13.0、zvariant_derive 5.13.0 | RUSTSEC-2026-0371、0370、0372 | Tauri 插件的 zbus 5 依赖链残留；迁移需要兼容升级上游依赖 |
| paste 1.0.15（worker） | RUSTSEC-2024-0436 | Laya/tokenizers 依赖链残留；保留已验证的 Laya 固定提交 |

保留公告不等于已确认可利用漏洞。当前兼容依赖约束下，未通过强行替换不兼容主版本、切入预发布依赖或修改审计忽略规则来清除提示。后续应随上游兼容迁移复查；本次扫描也不覆盖 Git 依赖的完整源码、Python 包、模型文件或 ONNX 二进制。

本次公告数据来源为 OSV 的 RustSec 镜像，原始公告详情、受影响范围和链接保存在 `fix-rust-osv-audit.json` 与 `fix-worker-osv-audit.json`。跨平台树检查为依赖解析结果，不是 macOS/Linux 编译或运行验收。

## 4. 验证结果

证据位于 `output/audit-2026-11-23/`。

| 检查 | 结果 | 证据 |
| --- | --- | --- |
| Rust 主库 | 1730 通过，0 失败，20 个既有测试保持 ignored | `fix-rust-tests.log` |
| Clippy lib/all-targets，`-D warnings` | 通过 | `fix-clippy.log` |
| 前端 Vitest | 58 文件、413 通过 | `fix-frontend-tests.log` |
| Laya worker | 3 通过 | `fix-worker-tests.log` |
| 原生 Windows WebView2 CSP | 1 通过，7 项布尔检查均 true | `security-webview.log` |
| 质量门 | 禁止项、官网检查、文件体积棘轮及基线校验通过；未抬高基线 | `fix-gates.log` |
| 已跟踪文件密钥扫描 | 0 命中 | `fix-secrets.log` |
| 新增源码及测试文件扫描 | 0 命中 | `fix-new-files-secrets.json` |
| registry/npm 依赖复审 | 见上节，停止维护公告保留 | `fix-*-audit.json` |
| 默认功能跨平台 chrono 引用检查 | 三个指定目标均无激活引用 | `fix-chrono-active-targets.log` |
| 差异空白检查 | `git diff --check` 通过 | 本轮命令输出 |

核心复现命令（在配置好的 Rust/Node 环境中）：

```powershell
Set-Location D:\ccfile\SynaRoute\src-tauri
cargo +1.99.0 test --locked --offline --lib
cargo +1.99.0 clippy --locked --offline --lib --all-targets -- -D warnings
cargo +1.99.0 test --locked --offline --manifest-path laya-worker/Cargo.toml
cargo +1.99.0 test --locked --offline --features native-acceptance,custom-protocol --test security_webview -- --ignored --nocapture
Set-Location D:\ccfile\SynaRoute
npm test -- --reporter=dot
npm run gates
npm run check:secrets
```

原生 CSP 测试需要 Windows WebView2 与已构建的 `dist`，使用独立临时 WebView 配置、最小测试 IPC，不加载用户 Store。新增 HTTP 测试只调用本机假上游与隔离凭据库。未调用真实收费模型，未操作用户会话库，未执行安装器、更新发布或 macOS/Linux 原生验收。
