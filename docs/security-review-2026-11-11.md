# SynaRoute 全链路安全与 Bug 审查

- 审查日期：2026-11-11，Asia/Shanghai。
- 基线：`07fd24363608904c0922c52968bd1af7ab40149f`，版本 `0.1.83`。
- 范围：当前工作区源码、配置、测试和依赖锁文件，不是仅审查最后一个提交。
- 方式：沿调用链静态检查，运行现有测试，并用临时回归用例验证可疑行为。
- 首次审查阶段的变更边界：不修复业务代码、不提交、不改分支。临时测试接线已撤回，证据保留在 `.build-tmp`；原有 `.laya-run.txt` 未改动。

## 修复跟进（2026-11-11，纳入 v0.1.84）

用户随后授权修复，并要求增加单 Key 会话测试。原始审查证据与结论保留在后文，以下记录此次改动：

| 问题 | 修复落点 | 安全边界 |
| --- | --- | --- |
| F1 | `crypto.rs`、`portable.rs` | 新备份 v4 使用 AEAD 附加数据认证格式版本与完整配置摘要，拒绝改配置后重算摘要及版本降级；旧加密备份默认拒绝，必须显式确认风险后才能兼容导入。 |
| F2 | `portable_credentials.rs`、`store.rs` | 在导入预检和持久化临界区均检查本机凭证的使用目标；拒绝改变绑定或借用孤儿凭证，不清空标志后偷偷复用，不靠删旧密钥解决，失败保留原配置和凭证。相同可信端点的正常恢复仍可保留本机密钥。 |
| F3 | `retrieval.rs`、`agent_tools/path_security_tests.rs` | 共享敏感路径判据检查 VCS 目录组件，真实路径经 canonicalize 后也会判定；读文件、图片和检索一致拒绝目录/文件链接别名。 |
| F4 | `mcp/logging.rs` | 成功和失败统一服从聚合详情开关；关闭时不记正文或可能回显正文的上游错误，开启时正文与错误均有长度限制。 |
| F5 | `mcp.rs`、`service.rs` | 绑定与句柄归属在同一临界区完成，同一请求端口复用回退监听器；启动/停止/重启编排串行化，丢弃句柄会取消任务。测试端口文件隔离在临时目录。 |

新增 `proxy/key_chat.rs` / `KeyChatDialog.tsx`：固定所选 Key 的多轮会话，三种协议、映射名、耗时/用量、停止/关窗取消；不切换 Key、不改健康状态、不保存对话正文，凭证只在后端读取。按 Key 卡片气泡按钮打开，说明见 `docs/key-conversation-test.md`。

正式回归已迁入仓库，不依赖 `.build-tmp` 接线。2026-11-11 最终自动验证结果：

- Rust 全量库测试：**1675 passed / 0 failed / 15 ignored**，包括 Windows 符号链接回归；最后一次使用 `cargo test --manifest-path src-tauri/Cargo.toml --lib --locked --offline` 复跑通过。
- 前端测试：**51 个文件 / 352 passed**；桌面与官网 TypeScript 类型检查均通过。
- 严格 Clippy：`cargo clippy --manifest-path src-tauri/Cargo.toml --lib --tests --locked --offline -- -D warnings` 通过。
- `npm run build`、Tailwind token 检查、`npm run gates`（forbidden / 官网 / ratchet / ratchet verify）均通过；构建仍有非阻断性分包提示。
- 对修复阶段的 39 个修改及新增文件单独扫描，**0 findings**；`git diff --check` 通过。单 Key 网络回归使用本地模拟上游。

修复阶段验证记录保留在 `.build-tmp/fix-20261111-rust-full.log`、`.build-tmp/fix-20261111-frontend-tests.log`、`.build-tmp/fix-20261111-clippy.log`、`.build-tmp/fix-20261111-build.log`、`.build-tmp/fix-20261111-gates.log`。该阶段最后一次 Rust 复跑结果由命令输出确认，未另存日志。

代码侧发布前复核（`v0.1.84`）：版本同步后再次通过 Rust 非测试构型检查、全目标严格 Clippy、两轮全量 Rust 测试（每轮 **1675 passed / 0 failed / 15 ignored**）、前端 **352** 项测试、桌面与官网类型检查、生产构建和全部质量门。记录保留在本机 `.build-tmp/release-0.1.84-*.log`；依赖锁文件仅同步本包版本，未更新依赖。

界面预览因本机 CUA 工具初始化失败未完成；不能将上述自动验证等同于原生窗口交互、真实供应商或跨机恢复验收通过。

范围说明：没有升级依赖；原报告中的 Rust 依赖公告匹配仍需核验，不能视为已修复。未使用真实收费 API Key 自动发请求，原生与跨机验收统一列在交接清单 C-62 / C-63。用户随后授权提交、推送及出包，本轮改动纳入 `v0.1.84`，同步更新版本字段与锁文件中的本包版本；发布通过标签触发既有 CI，须以远端构建、签名及安装器冒烟结果确认交付。原有 `.laya-run.txt` 保留。

## 结论

**确认 5 项问题：4 项 P1、1 项 P2。** 下列五项均有隔离用例支持，不将推测或依赖数据库匹配直接计入这个数字。

| 编号 | 优先级 | 问题 | 主要后果 |
| --- | --- | --- | --- |
| F1 | P1 | 加密备份没有认证外层路由配置 | 不解密密钥也能篡改接收凭据的上游地址 |
| F2 | P1 | 无密钥备份可以给新地址绑定本机旧凭据 | 导入后将原有 API Key 用于改变后的端点 |
| F3 | P1 | 目录符号链接绕过 VCS 文件禁读 | 被禁止的 `.git/config` 可进入模型工具结果 |
| F4 | P1 | MCP 无视详细日志开关保存正文 | 两个开关都关闭时仍明文落盘 prompt |
| F5 | P2 | MCP 并发启动丢失监听器句柄 | 停止后仍存在可连接、未被管理的服务端口 |

P1 表示应优先修复的安全问题，并不表示无需用户交互即可远程利用。各项具体前提见下文。本轮没有确认 P0；这不是“无 P0 漏洞”的保证。

另有 **20 个 Rust 依赖包命中 OSV 公告记录**，详见“依赖审查”。尚未完成公告明细、修复版本及逐项可达性核验，不能把它们等同于 20 个已确认可利用漏洞。

## F1 — P1：加密备份未认证路由配置

**代码位置**

- `D:/ccfile/SynaRoute/src-tauri/src/portable.rs:236`：计算独立的 `payload_sha256`。
- `D:/ccfile/SynaRoute/src-tauri/src/portable.rs:273`：仅将密钥集合交给 `crypto::seal`。
- `D:/ccfile/SynaRoute/src-tauri/src/portable_digest.rs:100`：通过可重新计算的摘要校验外层配置。
- `D:/ccfile/SynaRoute/src-tauri/src/crypto.rs:297`：AEAD 的认证范围没有包含外层 payload。

**触发前提**

攻击者能够替换或修改用户将要导入的加密备份，用户随后输入正确导出口令并确认导入。攻击者不需要知道口令，也不需要提前知道目标机器已有的 Key。

**调用链与根因**

`build_export → 独立明文 payload 与加密 secrets → parse_and_verify → crypto::open → apply_imported_config / save_credential`。

`payload` 中的 `baseUrl` 等安全相关配置没有与密钥段进行认证绑定。修改该字段并重算普通 SHA-256 后，原来的加密密钥段完全不变，摘要验证和密钥解密仍都成功。普通摘要只能检查一致性，不能证明配置来自加密备份的原作者。

**隔离验证结果**

1. 从临时源 Store 导出带合成 API Key 的加密备份。
2. 保持 `secrets` 的序列化字节不变，仅修改 `baseUrl` 并重新计算 payload 摘要。
3. 在另一份空白临时 Store 上输入原口令导入。
4. 导入成功；新端点与原 API Key 同时存在于目标 Store。

因此，随后使用该 Key 的转发或健康检查可以把凭据发给被替换的端点。验证没有向真实上游发送请求，也没有破解加密算法。

**修复建议**

为新备份格式将规范化 payload、格式版本和密钥段共同纳入认证：可以整体加密，或把 payload 的规范化字节/摘要作为 AEAD 的附加认证数据。必须在任何配置落盘前验证认证绑定。兼容旧格式时应明确提示它的外层配置未经认证，而不是宣称已完成防篡改校验。

回归要求：修改加密备份的 `baseUrl`、凭据引用或路由配置，即使重算普通摘要、保留原密文，也必须在写盘前被拒绝。

## F2 — P1：无密钥导入会复用改变端点后的旧凭据

**代码位置**

- `D:/ccfile/SynaRoute/src-tauri/src/store.rs:1271`：按相同 Key ID 覆盖配置，没有校验旧凭据与新端点是否仍匹配。
- `D:/ccfile/SynaRoute/src-tauri/src/store.rs:1403`：对账会将存在旧凭据的 Key 重新设为 `has_secret = true`。
- `D:/ccfile/SynaRoute/src-tauri/src/portable.rs:333`：只有名称和地址同时变化才生成可疑冲突提示。
- `D:/ccfile/SynaRoute/src-tauri/src/proxy.rs:1763`：转发按 Key ID 读取凭据。

**触发前提**

导入文件含有目标机器已存在的 Key ID，但改变了其上游地址；可保持名称不变。拿到用户分享的无密钥备份即可知道其中的 ID，不需要知道 API Key 明文。

**调用链与根因**

`无 secrets 的备份 → preview_import → apply_imported_config → 保留原 secrets.enc 条目 → reconcile_has_secret_flags → 转发使用原凭据`。

配置身份直接由导入文件的 ID 决定，凭据却从本机旧库继续沿用。即使导入方明确填入 `hasSecret: false`，对账仍会将其改回 true。只更改端点、保持名称不变时，也不会触发 `suspicious_conflicts` 的逐条地址警告。

**隔离验证结果**

临时 Store 中已有可信端点及合成凭据。将其无密钥导出文件中的端点改成另一地址，保持 ID/名称不变、设置 `hasSecret: false` 后执行合并导入：预览没有可疑冲突，导入成功，旧凭据仍可通过导入 Key 的 ID 取得。

**修复建议**

将凭据绑定视为独立安全决策。无密钥导入改变规范化端点或凭据引用时，默认不沿用本机凭据；若支持迁移，应提供明确的逐项确认，而不是仅依赖普通“导入配置”确认。检查余额面板、判断服务等辅助凭据的同类引用。不能只修改 `has_secret` 标志，否则对账仍会重新绑定旧条目。

**与 F1 的区别**：F1 在空白目标机器上也成立，问题是加密备份的认证范围；F2 不需要加密段，问题是无密钥导入对目标本机凭据的授权。只修复其中一项不能消除另一项。

## F3 — P1：真实路径未复查 VCS 禁读规则

**代码位置**

- `D:/ccfile/SynaRoute/src-tauri/src/agent_tools.rs:198`：仅对请求中的相对路径执行 `is_vcs_internal`。
- `D:/ccfile/SynaRoute/src-tauri/src/agent_tools.rs:221`：解析真实路径后只进行凭据文件名和硬链接检查。
- `D:/ccfile/SynaRoute/src-tauri/src/retrieval.rs:283`：凭据名称检查不识别 `.git` 父目录，普通叶子名 `config` 会通过。

**触发前提**

被检索项目内存在指向自身 VCS 目录的符号链接，并诱导模型读取该别名。Windows 实验需要符号链接创建权限；其他平台是否具备相同利用条件还需要对应平台验证。

**隔离验证结果**

在临时项目中写入仅含合成标记的 `.git/config`，并建立 `project-meta → .git` 目录链接：

- `read_file(".git/config")` 被拒绝。
- `read_file("project-meta/config")` 成功，工具结果包含 `.git/config` 中的合成标记。

这条路径仍位于工作目录内，因此现有工作根检查不会拒绝它；解析后的文件名仍是 `config`，敏感文件名检查也不会拒绝。工具结果后续会被送入聚合模型会话。如果真实 Git 配置包含远端凭据，就存在外发风险。

**修复建议**

在规范化工作根与候选路径之后，对真实的工作根相对路径再次应用 VCS 禁读规则；将该规则纳入统一的可读路径检查，覆盖直接读、图片和其他可能读取文件正文的入口。补充文件链接和目录链接两类测试。

**未扩大结论**：同轮尝试的目录别名写入 `.git/hooks` 用例没有写入成功。本报告只确认 VCS 文件禁读绕过，不据此宣称已经实现任意代码执行。

## F4 — P1：MCP 在关闭日志详情后仍保存正文

**代码位置**

- `D:/ccfile/SynaRoute/src-tauri/src/mcp.rs:703`：成功分支无条件构造完整 RequestTrace。
- `D:/ccfile/SynaRoute/src-tauri/src/mcp.rs:724`：失败分支同样无条件保存 prompt。
- `D:/ccfile/SynaRoute/src-tauri/src/store.rs:650`：事件进入文件日志。
- `D:/ccfile/SynaRoute/src-tauri/src/store.rs:707`：整个事件被序列化，没有在写入层按开关剥离正文。

**触发前提**

使用 MCP `synaroute_ai` 工具。不需要启用“记录调用模型日志”或“记录大脑聚合详情”。

**隔离验证结果**

创建默认 Store，确认 `aggregate_trace_enabled = false` 且 `request_log_enabled = false`；在聚合尚未启用、调用直接失败的情况下，提交带合成敏感标记的 prompt。执行日志 flush 后，临时日志文件仍含该完整标记。

成功分支还有同样的无条件写入路径，会保存 prompt 与聚合结果。关闭两个详细日志开关不能阻止这些正文进入磁盘和内存事件。

**修复建议**

让 MCP 成功与失败分支统一遵守聚合详情开关：关闭时只保留元信息，开启时也对正文做体积限制。为所有事件入口补充“详情关闭时不落正文”的集成测试。修改开关逻辑不会自动清除既有日志，应单独提醒用户检查历史日志，不擅自删除。

## F5 — P2：并发启动 MCP 后停止不彻底

**代码位置**

- `D:/ccfile/SynaRoute/src-tauri/src/mcp.rs:234`：检查运行状态后在锁外执行监听绑定。
- `D:/ccfile/SynaRoute/src-tauri/src/mcp.rs:273`：创建监听任务后直接覆盖运行句柄，没有进行第二次竞争检查。
- `D:/ccfile/SynaRoute/src-tauri/src/mcp.rs:291`：停止只处理最终保存的句柄。
- `D:/ccfile/SynaRoute/src-tauri/src/service.rs:794`、`D:/ccfile/SynaRoute/src-tauri/src/service.rs:906`：注册、启用等入口没有共用的生命周期串行锁。

**触发前提**

多个启动、注册或重启操作发生重叠。例如启动恢复尚未完成时，另一个异步入口要求启动服务。

**隔离验证结果**

8 个任务同步请求同一个普通非零端口；端口竞争后部分调用使用回退端口。全部返回后调用 `stop()`，等待 100 ms 再连接返回的端口，该次实验仍有 **6 个不同端口可连接**。

新句柄覆盖旧句柄后，旧监听任务没有被停止。界面显示停止或停用 MCP 时，失去管理的服务仍可能接收请求，继续访问同一个 Store 和聚合功能。只有退出进程才能可靠结束这些残留任务。

**修复建议**

为 `start / stop / restart` 建立统一的生命周期串行化或代际检查；未获管理权的监听器和任务必须主动关闭。可为运行句柄增加 drop 清理作为兜底，但仅靠 drop 不能解决返回失效端口等并发语义问题。测试应覆盖同一非零端口并发启动、启动与停止重叠、重启后旧端口不可连接。

## 依赖审查

### 已完成的检查

| 对象 | 检查 | 结果 |
| --- | --- | --- |
| 桌面前端 | `npm audit --json` | 0 条漏洞报告 |
| 官网 | `npm audit --json` | 0 条漏洞报告 |
| Rust 锁文件 | 按 crate 名称和精确版本请求 OSV `querybatch` | 检查 615 个 registry 包；20 个包命中，合计 38 个不同公告 ID，尚未合并别名 |

查询地址：`https://api.osv.dev/v1/querybatch`。查询时间为 2026-11-11 14:26（Asia/Shanghai）；仅发送公开的依赖名与版本，没有上传项目源码或凭据。

本次匹配涉及：`event-listener`、`glib`、`h2`、`libsqlite3-sys`、`proc-macro-error`、`quinn-proto`、`rustls`、`rustls-webpki`、`tokio`、`unic-char-property`、`unic-char-range`、`unic-common`、`unic-ucd-ident`、`unic-ucd-version`、`webpki-root-certs`、`webpki-roots`、`zbus_names`、`zip`、`zvariant`、`zvariant_derive`。

对以下包补充核对了当前 Windows 目标的普通依赖树，它们不是仅出现在锁文件里的无关条目：

| 依赖 | 锁定版本 | 示例匹配公告 | Windows 普通依赖链 |
| --- | --- | --- | --- |
| `rustls` | `0.23.42` | `RUSTSEC-2026-0285` 等 | `reqwest → hyper-rustls / tokio-rustls → rustls`，代理和更新器均引入 |
| `h2` | `0.4.15` | `RUSTSEC-2026-0258` | `hyper → h2`，项目与 reqwest 引入 |
| `libsqlite3-sys` | `0.30.1` | `RUSTSEC-2026-0459` | `rusqlite → libsqlite3-sys` |
| `zip` | `4.6.1` | `RUSTSEC-2026-0367` 等 | `tauri-plugin-updater → zip` |

**重要限制**：属于普通依赖树不等于公告所述漏洞路径一定可达。OSV 批量接口只返回匹配编号；后续公告详情请求连接失败，提权重试又因自动审批服务连接中断而未执行。因此本轮没有确认各项的漏洞类型、撤销状态、受影响特性、修复版本或利用前提，也没有将它们列为 F1–F5 那样的已验证缺陷。

后续应优先取得这些公告的完整内容，去除别名重复，区分安全漏洞与维护性告警，再结合目标平台、Cargo feature 与实际调用点决定升级范围。不要仅凭 `npm audit` 为零认定整个 Rust 桌面应用的依赖安全。

## 测试和证据

| 检查 | 结果 |
| --- | --- |
| 原始源码 `cargo test --lib --locked --offline` | 审查前后各运行一次；均为 1,658 通过、0 失败、15 忽略 |
| `npm test -- --reporter=dot` | 50 个测试文件、342 项通过；初次受 esbuild 沙箱子进程限制，获准重试后通过 |
| 桌面前端 TypeScript | `tsc --noEmit` 通过 |
| 官网 TypeScript | `tsc --noEmit -p site/tsconfig.json` 通过 |
| 临时定向回归用例 | 分轮执行 6 个不同用例：5 个安全/正确性断言失败并支持 F1–F5；1 个写入对照通过 |
| `git diff --check` | 临时接线撤回后通过 |
| `npm run gates`、独立 `check-secrets` | **未完成**：Node 启动 Git 子进程时报 EPERM；沙箱外重试因自动审批服务连接中断未执行，不能报告为通过 |

新增回归用例只操作临时目录、合成凭据和本机临时端口；没有发送真实上游请求，没有执行真实 Git hook。

证据文件：

- `D:/ccfile/SynaRoute/.build-tmp/review-20261111-rust-tests.log`：原始测试基线。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111-rust-final.log`：撤回测试接线后的 Rust 回归结果。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111-regressions-escalated.log`：VCS 读取绕过、MCP 和无密钥导入的验证记录；写入对照通过。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111-regressions-final.log`：普通非零端口 MCP 竞态、日志开关、两种备份缺陷的最终验证记录。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111/agent_regressions.rs`、`D:/ccfile/SynaRoute/.build-tmp/review-20261111/mcp_regressions.rs`、`D:/ccfile/SynaRoute/.build-tmp/review-20261111/import_regressions.rs`：回归用例。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111/reproduce.patch`：临时测试接线补丁，当前未应用。需同时保留上述测试文件才可重新接入执行。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111-npm-audit.json`、`D:/ccfile/SynaRoute/.build-tmp/review-20261111-site-audit.json`：两份 npm 审计结果。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111/rust-osv-audit.json`：完整 OSV 匹配结果。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111/audit_rust_dependencies.mjs`：本次 Rust 依赖批量查询脚本。
- `D:/ccfile/SynaRoute/.build-tmp/review-20261111-quality.log`：未完成门禁的环境错误记录。

上述 `.build-tmp` 证据未加入 Git；如需交接或 CI 复现，应另行保存到团队认可的位置。不要把这些刻意断言失败的回归用例误认为原有测试本轮发生了回归。

## 覆盖范围与未完成项

本轮重点走查了以下边界，而非只做关键词扫描：

- React 状态与 IPC 桥接、Tauri 配置/能力、深链接导入确认。
- HTTP 监听、LAN 令牌、Origin 与请求体限制、路由和候选选择、故障转移及取消生命周期。
- 上游 URL、鉴权头、重定向策略、流式与非流式响应边界和协议转换关键入口。
- MCP HTTP/stdio、聚合调用、只读检索工具、规范化路径与写入保护。
- 密钥库接口、加密信封、导入导出、余额凭据引用、配置持久化和回滚备份。
- Codex 会话文件/SQLite 的删除与备份路径、原生进程接入、Laya 安装及发布相关脚本。
- 官网 Markdown 清洗、两个前端工程与 Rust 依赖锁文件。

**未完成或不应推断的范围**：没有逐行形式化验证全部 164 个 Rust 文件和 156 个桌面 TS/TSX 文件；没有进行真实厂商计费请求、真实用户数据破坏性验证、GUI 全流程回归或 macOS/Linux 真机复现；没有进行大规模模糊测试；Rust 公告明细及独立秘密扫描/完整门禁未完成。

现有测试全绿只说明这些既有用例通过，不能覆盖本次新增的攻击路径。

## 建议处理顺序

1. 优先修复 F1/F2 的备份认证与凭据绑定，再处理 F3/F4 的读取边界和默认正文落盘。
2. 修复 F5 的生命周期竞争，保证“停止服务”的操作确实关闭所有监听器。
3. 恢复公告详情和质量门禁核验，按实际可达性处理依赖告警。
4. 将定向用例整理进正式测试后，重新运行完整回归，并补各平台验证。

修复前的临时防护：不要导入来源不可信或可能被替换的备份；不要对含不可信链接的项目启用自动检索；不要依赖当前 MCP 详细日志开关保护正文；若发现 MCP 关闭后仍能连接，完全退出应用以终止进程内的残留监听器。涉及真实敏感数据的历史日志由用户确认后再清理，不在本轮自动删除。
