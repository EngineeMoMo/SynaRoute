# 后续审查修复记录（2026-11-23）

针对修复后复核的 R1、R2、R3 以及两项工程改进，五项均已落实，纳入 0.1.99 发布准备。本文记录修复阶段的验证结果，实际发布状态以 Git tag 和 GitHub Release 为准。原始复现证据保留在 `output/audit-2026-11-23/followup-review.md`，其中的探针通过表示当时成功复现缺陷，不代表当前仍存在。

v0.1.97 发布前的 CI 发现 LAN 回归夹具依赖系统对特定地址与通配地址的端口冲突规则，以及 `127.0.0.2` 回环行为，已取消该版发布并保留原标签。后续用例改为占用通配地址、避开运行中的代理端口，分别验证部分失败后的真实连接及监听地址，继续保留失败必须返回错误的断言。从 v0.1.98 起，发布流程要求 macOS 和 Linux 的完整 Rust 库测试通过后才出包。

v0.1.98 的 macOS 完整测试和 Windows 第一轮完整测试通过，Windows 第二轮捕获既有会话扫描用例的时间戳偶发：等长重写的文件修改时间可能相同。该版被质量门阻止发布。v0.1.99 将夹具改为每次改变文件长度，同时固定修改时间，确定性验证持续变化的快照不能写入缓存；保留返回记录、重试次数、磁盘与内存缓存等全部断言。

## 修复行为

| 项目 | 最终行为 | 正式回归证据 |
| --- | --- | --- |
| R1 / P1：模型拉取、健康探测及聚合错误泄漏凭据 | 上游共享 `ErrorRedactor` 在解码 JSON 后清理字段名、嵌套值及数字形式的凭据；主代理 SSE/HTTP 共用同一实现。模型拉取、补全、聚合工具会话的 HTTP 错误、200 错误对象及解析失败统一脱敏。无法安全解析的 JSON、带转义的畸形正文只返回通用诊断 | `upstream/error_redaction_tests.rs` 使用真实本机 HTTP 上游覆盖三种协议、401/200、健康事件日志、工具会话；原 SSE 拆块和协议转换回归继续通过 |
| R2 / P1：LAN 部分失败时显示关闭 | 开关写入及状态回读进入同一串行队列。成功和失败均读取实际设置及三个分类运行状态；失败展示实际状态和原因，不再恢复旧快照。操作期间禁用开关；配置无法读回时展示状态未知并提供重试关闭 | `lib/lanSettings.test.ts` 覆盖部分成功、连续开关顺序、回读失败及恢复；`proxy/active_requests_tests.rs` 注入一类端口全部占用，验证另一类仍运行时后端实际配置可被正确读取 |
| R3 / P2：换凭据/租户后旧引用继续发送 | 路由指纹包含有效上游身份摘要，自定义头也纳入配置指纹；服务层凭据变化及上游身份字段变化提升代次并清除会话/引用。发请求前再次校验实际凭据及 Key 快照，避免准备和发送之间更换身份；旧代次晚到响应不能恢复续传登记。同值保存和主口令重加密保留相同上游身份 | `routing/upstream_identity_tests.rs` 覆盖真实 JSON/SSE 续传、凭据/租户切换及改回、准备后修改、晚到响应、导入路径写凭据、同值保存、名称编辑和主口令加密 |
| codegraph 后代进程清理 | Windows 在暂停状态启动进程，加入设置了关闭即终止的 Job Object 后恢复执行；Unix 建立独立进程组。超时、输出超限、任务取消和父进程正常退出均清理所属进程树；正常退出时先清理后代再完成管道读取，避免继承管道拖住结果 | `codegraph_output_tests.rs` 覆盖两路输出上限、继承管道，以及超时/取消/父进程成功后的后代心跳停止 |
| CSP 自动验收 | Windows Gates 在构建前端后显式运行 `security_webview`，启用 `native-acceptance,custom-protocol` 和 `--ignored`；原生测试仍不混入普通库单测 | 同一命令已在本机 Windows WebView2 执行，七项检查全部通过；工作流尚未推送触发 GitHub 运行 |

## 最终验证

环境：Windows，Rust 1.99.0，Node.js 22.9.0。日志均在 `output/audit-2026-11-23/`。

| 检查 | 结果 | 日志 |
| --- | --- | --- |
| Rust 主库完整测试 | 1737 通过，0 失败，20 个既有 ignored 保持原状态 | `followup-fix-rust-tests.log` |
| Clippy lib/all-targets | `-D warnings` 通过 | `followup-fix-clippy.log` |
| 前端完整测试 | 59 个文件，416 通过 | `followup-fix-frontend.log` |
| TypeScript / Vite / Tailwind 产物检查 | 通过 | `followup-fix-build.log` |
| 续传上游身份专项 | 3 通过，含真实 JSON/SSE HTTP | `followup-fix-identity.log` |
| codegraph 输出及进程专项 | 6 通过 | `followup-fix-processes.log` |
| 原生 WebView CSP | 1 通过；资源、React、数据图片可用；内联脚本、data 脚本、eval、外联被阻止；IPC 回传成功 | `followup-fix-security-webview.log` |
| 项目质量门 | 禁止项、官网、棘轮及基线校验均通过；基线零改动 | `followup-fix-gates.log` |
| 已跟踪文件密钥扫描 | 599 文件，无发现 | 包含于 `followup-fix-gates.log` |
| 新增源码和测试密钥扫描 | 16 文件，无发现 | `followup-fix-new-secrets.json` |

本轮未重复联网依赖扫描；依赖维护公告及第一轮 worker 的验证结果仍见前一份记录。未调用真实供应商或改写用户客户端配置。macOS/Linux 原生运行、安装器及 GitHub 托管环境的实际工作流运行未在本机验证。Unix 进程组清理不等于收容主动脱离进程组的任意程序。
