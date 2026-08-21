# SynaRoute 项目须知

Tauri 2 桌面应用（Rust 后端 `src-tauri/` + React/TS 前端）。代理路由 AI API 请求，含故障转移、模型映射、协议转换、加密密钥存储、健康检查。

## 🔴 换机/接手先读这一篇

**[docs/14-交接与待办清单.md](docs/14-交接与待办清单.md)** —— 2026-07-31 换机交接。里面有：

- **已修完（换机后）**：重复接入冲掉「接入前备份」（P0 数据丢失级）、短路窗口测试串台 + 重启代理不解除窗口、
  桌面端还原非原子、429 `Retry-After` 不透传、全失败返 502 应为 529、
  **桌面端模型名硬过滤**（对外名不合规会导致模型选择器为空 + `ModelsNotDiscoveredError`，
  已在保存 Key 时拦截；注意 `claude-synaroute-` 前缀对桌面端**无效**，详见文档第九节）。
  当前基线 `cargo test --lib` **311 passed / 0 failed，连跑 5 次全绿**
- **审查已补完**：docs/14 第五节原「未系统覆盖」的 5 个区域全部查完，无缺陷级问题；
  顺手加固 2 处（`parse_and_apply` 加 canonicalize 防线堵符号链接逃逸、
  `save_brain`/`save_settings` 改走带回滚的 `mutate_and_persist`）
  <br>⚠️ **勘误（2026-08-03 评审实测）**：当时未清点其余写路径，`store.rs` 仍有 **12 处**裸
  `self.persist()`（落盘失败即「内存领先磁盘」，该方向永不自愈）。清单与优先级见
  docs/14 第六节勘误框与 docs/15 P1-2。`update_health` 是刻意例外，勿统一。
  <br>✅ **已闭环（2026-08-22 复核）**：现存裸 `self.persist()` 只剩 **3 处**，且都不是问题 ——
  两处在 `mutate_and_persist` / 带回滚的那个变体**内部**（就是那个唯一落盘点本身），
  一处是 `flush_health_if_dirty`（文档里写明的刻意例外）。上面那句「12 处」勿再当待办。
- **P2 九条已处理完**（见 docs/14 第四节结论表）：`supports1m` 改按 contextWindow 判定、
  cc-switch 接管检测 + UI 常驻警告、加 `anthropicFamilyTier`/`isFamilyDefault`；
  `disableDeploymentModeChooser` 保持对齐 cc-switch，`headers_json` 死字段暂留
- **FR-025 开机自启动已接线**（见 docs/14 第十节）：原是个**静默失效的开关**——字段存在、
  UI 能点、值落盘，后端却从无代码注册自启动项。已接 `tauri-plugin-autostart`，含启动时状态对账
  （老用户开关开着但系统里没项）与「仅 `--autostart` 参数才最小化到托盘」。
  当前基线 **312 passed / 0 failed，连跑 5 次全绿**
- **FR-021 配置导入/导出已实现**（原是设置页两个 `disabled` 按钮）：新增 `crypto.rs`
  （Argon2id + AES-GCM 口令信封）+ `portable.rs`（导出/sha256 校验/Merge 与 Replace 两种导入）。
  关键判据：密钥不能照搬 DPAPI 密文（绑账户、换机解不出），故含密钥导出走**用户口令**重新加密；
  端口/日志目录/MCP 注册态属本机运行态不随导出走
- **FR-022 托盘补齐**：图标按代理运行状态灰度化派生（从运行时图标现场派生，换图标自动跟随）、
  「代理」子菜单三分类启停（**语义与界面按钮一致**：起=顺带写工具配置、停=顺带还原，
  否则会出现「托盘说已启动、客户端没走代理」）、「主 Key」子菜单快切。
  重排规则收归后端 `Store::set_primary_key` 做单一事实来源，前端改调它（原先前端自己算，
  托盘再写一份必然漂移）
- **主口令增强模式已实现**（FR-018 可选增强，原是 `disabled` 开关 + 零使用点字段）：
  **模式的事实来源是 `secrets.enc` 里有无 `master` 头部**，`settings.master_password_enabled`
  只是 UI 镜像（启动时以库为准对账，且 `save_settings` 不许前端覆盖它——否则自造
  「配置说开着、库里没头部」的死局）。`crypto.rs` 另加长驻密钥 API（解锁派生一次、
  各条只做 AES-GCM，因为转发每次取密钥都要解）+ 校验串（空库也能判口令对错）。
  三个迁移操作都是「先整库解密成功 → 备份 → 写盘 → 失败回滚内存」。
  **锁定态四处刻意行为**：`get` 返 Err 不返 `Ok(None)`、`set` 拒写不回退 DPAPI、
  跳过 `has_secret` 对账、跳过健康探测
  当前基线 **368 passed / 0 failed，连跑 5 次全绿**；`--test-threads=1` 全量也全绿。
- **第二轮审查已完成（2026-08-01）**：五路并行审查 + 自审 lib.rs/tools/store，修出
  **P0 一条：切主题/语言会把刚关掉的开机自启动重新装回系统**（`store.settings` 是挂载时的旧快照，
  而后端只对 `mcp_*`/`active_*`/`proxy_ports` 有「保留后端值」防线，`auto_start` 没有 →
  已改为落盘前先拉磁盘权威值，见 `store.ts persistOneSetting`）；
  P1 一条：`aggregate.rs` 路径遏制有两条绕过（多一级路径穿透链接目录 / 目标本身是符号链接）；
  P2 两条：`Retry-After` 取最大值让一个撞配额的 Key 拖垮整池（改取最小值）、硬错误（401 等）
  被包装成 529「过载请重试」（改按状态码分流，4xx 原样回、不武装短路窗口）；
  P3 两条 + 前端一致性 5 条。**注意 `all_failed_gate_remaining` 里的 `.max()` 是对的**，
  与上面那条不矛盾（它比较同一结论的两个下限，都是「不早于」）
- **效率整治（2026-08-01）**：`list_all_events` 原先每 2s 返回 500 条**含 trace 正文**
  （满载 19 MB → 572 MB/min），已改为列表只带 `hasTrace`、展开时按 id 单取，**降 99.5%**；
  转发热路径去掉「开关关着也 pretty-print 整个请求体」与「每请求克隆整份 AppSettings 3~4 次」；
  LogsPage 5 趟遍历合成 1 趟
  <br>⚠️ **勘误（2026-08-03 评审实测）**：pretty-print 那条**只修了 `downstream_body`**
  （`proxy.rs:457`）；`forward_to_key`（`:1384`）与 `try_stream_to_key`（`:1249`）仍无条件
  `to_string_pretty`，`:749`/`:774` 还无条件全量复制响应体。默认关日志时每请求白做
  ≈0.6~1 MB 分配。另 `list_all_events` 降的是**过 IPC 的字节数**，服务端 `store.rs:344`
  的 `..e.clone()` 仍在深拷贝 trace 正文后丢弃。见 docs/15 P1-5 / P1-6。
  <br>✅ **已闭环（2026-08-22 复核，逐处看过代码）**：`proxy.rs` 生产路径上的
  **全部 4 处** `to_string_pretty` 都在 `if req_log` 里（含本轮新加的
  `StreamAttempt::HttpError.request_body`），注释也写了「三处同进同退」；
  `strip_trace` 已改为逐字段构造，函数上方明确写着「绝不能用 `..e.clone()`」并给了
  19 MB/轮 的账。上面那段勘误勿再当待办。
- **v0.1.6 已 bump 并出包**：三处版本号一致，`SynaRoute_0.1.6_x64-setup.exe`（5.99 MB）+
  exe（23.9 MB）已产出，chunk 嵌入判据各 = 1、与源产物 sha256 一致，已复制到 `F:\SynaRoute\`
  （旧 exe 已备份）。**仍未验证：NSIS 安装器能否装成**（历史上静默安装卡死过两次，
  此前部署都是绕过安装器直接覆盖 exe；F 盘现在放的也是直接覆盖的）
  当前基线 **383 passed / 0 failed，连跑 3 次全绿**
- **仍未做**：真机验证清单（docs/14 第八节，15 条）、提交与发 Release
- **大脑聚合多模态 + 工具调用已实现**（2026-08-01，FR-026 / FR-027，详见 docs/14 第一节末专节）：
  参与者可用一组**只读**工具（`read_file`/`grep`/`list_dir`/`codegraph_query`）按需检索，
  开关默认**关**（每轮重发完整历史，额度消耗显著更高）；MCP `images` 参数支持传图
  （相对 cwd、≤4 张、单张 ≤5MB、png/jpg/gif/webp）。
  **三道路径防线恒定生效**：拒 `..`/绝对路径 → canonicalize 后须仍在工作目录内 →
  凭据类文件一律拒读（按「模型给的名字」与「解析链接后的真实落点」**各判一次**，两次都不能省）。
  每条防线都做过故障注入验证（去掉后测试必须变红）。当前基线 **433 passed / 0 failed**
  <br>📌 **基线口径（2026-08-22 实测）**：上面各条历史行里的 311/312/368/383/433/506 都是**当时**的
  数字，勿当现值。当前实测 `cargo test --lib` = **708 passed / 0 failed**
  （连跑 12 次全绿；`cargo clippy --lib --all-targets` 零警告；`tsc --noEmit` 干净；
  `npm run build` 通过含颜色类零 CSS 检查）。
  接手时请自己跑一遍取当前值，不要引用本文档里的历史数字当基线。
- **docs/15 第二三批已实施完 + 第二轮对抗式审查已闭环（2026-08-04）**：详见
  [docs/15 第七节及「七之二」](docs/15-架构评审报告.md)。本轮修掉一条 **P1 数据丢失**
  （Replace 导入删旧密钥却只备份 config.json → 用户回滚配置后密钥永久没了）与两条
  **切分类串台**（在途 IPC 结果写进新分类页，其中桌面端那条因轮询已停表而**永不自愈**）。
  <br>⚠️ **新记一类静默失效**：Tailwind 透明度修饰符只认标度内的值（3.4 是 5 的倍数），
  写 `bg-warning/8` 不报错、**直接一条 CSS 都不生成**。全仓 20 处这么写（含 `Badge`
  除 neutral 外的全部彩色变体），一直是「有边框有字色、独独没底色」而无人发现。
  已在 `tailwind.config.js` 补进 8 / 12 两档（不是把类名改成 /10，要保住设计稿原值）。
  判据：`npm run build` 后 `dist/assets/*.css` 里能搜到 `.bg-warning\/8`。
  <br>**刻意未做**（别当遗漏）：P2-1 `upstream/` 目录化、P2-7 `service.rs` 抽出、
  P2-8 `CategoryType` 查表化、`AppSettings` 拆 `UserPrefs`/`RuntimeState` —— 四项都是
  纯结构调整、零用户可感收益，却是 diff 最大 / 风险最高的，不该紧挨着一次远程真机测试做
- **尚未开发的功能：已全部做完**（FR-001~027 逐条核过，无遗留项、无第二个静默失效开关）。
  docs/13「非 GPT 主 Key 支持 Codex 工具」也**已闭环**：从 Codex 自己的日志库
  （`~/.codex/logs_2.sqlite`）取证到 opus 主 Key 下 **9/9 次调用全走 `exec`**、
  MCP 与 `tool_search` 链路都通，故原计划的「沙箱工具拍平」**不做**（前提被证伪，详见 docs/13 第十一节）。
  顺带修掉一处协议映射偏差：`developer` 角色原被降级成 user，现与 `system` 并列
  —— Codex 的 **skills 就装在 developer 消息里**（不是工具），降级会削弱「必须使用该 skill」这类强指令
- **第三轮审查已清空（2026-08-22，v0.1.29 之后）**：17 条确认缺陷全部修完，每条都做过
  **故障注入**（去掉修复后对应测试必须变红，没红的当场重做注入手法）。按「用户看得见什么」分类：
  - **静默给错结果**：结构化输出约束（`response_format` ↔ `text.format`）跨协议被整个丢掉
    → 请求 200、模型回散文、客户端 `JSON.parse` 炸掉而日志毫无线索；Kimi 余额有端点无字段
    （snake_case `available_balance` 缺失，camelCase 那个是 Novita 的、量纲还不同，**别合并**）。
  - **数字会当着用户的面变小/变错**：90 天滚动删桶让「累计用量」每 90 天掉一截
    （新增持久化的 `retired`，快照版本 2→3）；v1 用量文件的 `entries` 重启一次就永久消失；
    「今日用量」比「累计」慢一分钟（未落盘增量没并进当天桶，已收进后端 `daily_usage_buckets`
    —— 只读视图**不得抬基线**，抬了那段就再也不落盘）；计费倍率填 `"inf"` 能把金额撑成
    $18446744073（`f64::INFINITY` 能过 `> 0.0` 那道门）。
  - **代理自己造成的失败算在上游头上**：被故障转移预算掐短的尝试计入熔断（排在预算末尾的
    候选每轮都被削短 → 池里最好的备用反而先熔断，系统性偏置）；`retry_after_hint` 只在
    「给了头的候选之间」取最小，没给头的不参与 → 一个撞配额的 Key 仍能把整池按 300s 停摆
    （同一误伤的第二条路径）。
  - **说一套做一套**：桌面端 MCP 只写 `Claude-3p/`，而「切回官方」会把 deploymentMode 复位成
    1p 却刻意保留 MCP 注册 → 那句「大脑聚合继续可用」是假的（改为两个部署目录都写，
    `is_mcp_registered` 用 **all 而非 any**）；导入配置带走 `proxy_running_categories`
    → 新机器上「用户从未点过启动」就替他改写了 `~/.claude/settings.json`；
    「测试查询」绕过 `isValidHttpUrl` 却照样落盘。
  - **排障时看到的是假现场**：流式失败的链路快照把下游原始请求体当成「上游请求」
    （跨协议时根本不是一回事，而 400 的成因几乎全在转换里）；折叠日志展开后看的是第 1 次
    的正文而表头是最近一次；短路窗口内每次客户端重发都记一条「已忽略熔断兜底重试」
    —— 那件事没发生，还把有用事件挤出 MAX_EVENTS 环。
  - **其余**：日志保留期只在启动时清（托盘常驻用户永不生效，已挂到写线程跨天重开那一刻）；
    上游按 `max_tokens` 截断在默认配置下完全不可见；新增 Key 全部 `priority=999`；
    桌面端还原中途失败丢 `.synaroute-created` 标记（不可自愈）；
    切分类时在途 `loadCategory` 把上一个分类的 Key 写进新分类页（`loadCategory` 无轮询纠正）。
  <br>**注入手法上踩过的两个坑**（写在对应测试注释里，别重犯）：① 想用「把文件换成目录」
  让 `remove_file` 失败，但 `with_rollback` 先对全部路径拍快照、对目录 `fs::read` 直接失败
  → 还没进 op 就返回，那个窗口压根不会打开（注入后测试恒绿 = 什么都没测到）；
  ② 想用打 500 把 Key 打到熔断，但 **5xx/429 刻意不计熔断**（不罚好 Key），永远打不出来。
  <br>**另修掉一个自己写的假红**：那条短路窗口用例最初按「整体计数比较」写，十轮里假红过一次
  —— 窗口只有 5s 而套件里几十个 tokio 用例并行，机器一卡就超窗。已改为**逐条判定**
  （只对响应体带短路专属文案的请求断言），并加「至少要有一次真被挡住」防止用例悄悄空转。
  <br>**并顺着另一次假红挖出一条真缺陷**：`ccswitch` 导入的库副本名是
  `synaroute-ccswitch-{pid}-{纳秒}`，而 `Utc::now().timestamp_nanos_opt()` 本机实测量化粒度
  **只有 100ns** —— 单线程连续读 20 万次有 5.3 万次与上一次完全相同（26%），
  8 线程并发 16 万采样只剩 1.9 万个不同值（**88% 撞车**）。撞名后一个的 `remove_file`
  会删掉另一个正在打开的副本，报 `unable to open database file`（套件约每 10 轮红一次；
  生产形态是用户连点两下「从 cc-switch 导入」，而那句报错把人指向「表结构可能已变」）。
  已加进程内自增序号。<br>⚠️ 这条的**测试也踩过坑**：最初写成「连续调 1000 次断言互不相同」，
  去掉序号照样绿 —— `db_copy_path` 自身开销就够拉开 100ns，单线程压根撞不上。
  改成并发 8×2000 后注入即红（16000 次只得 15179 个不同名）。
  **教训：测并发缺陷的用例必须并发地写，顺序版给的是假的安全感。**
- **刻意不修的项**（别当成遗漏重复劳动）：SmartScreen 签名告警、`retrieval.rs` 的 `cwd` 白名单、
  请求日志存明文对话（默认关闭）
- **换机注意**：`secrets.enc` 由 DPAPI 绑账户、**不可跨机器搬运**；本文档里的绝对路径都是旧机器实测值
- **判据取证方法**：如何反查 `claude.exe` / `codex.exe` 的字段与内嵌官方 gateway 规范
  （本轮所有「客户端认什么字段」的结论都出自此，不是文档推测）

## ⚠️ 构建/部署硬规则（踩过坑，务必遵守）

**生产 exe 必须用 `tauri build`，禁止裸 `cargo build --release`。**

裸 `cargo build` 不会嵌入前端资源，产出的 exe 运行时去连 `localhost:1420`（devUrl），生产环境无 dev server → `ERR_CONNECTION_REFUSED`，界面打不开。

```bash
npm run tauri build              # 出 NSIS 安装包（交付）
npm run tauri build -- --no-bundle   # 只出 exe（快速验证）
```

**部署前必须用可证伪证据验证前端已嵌入**：`dist/assets/` 的 chunk 名要能在产物 exe 里 `grep -c` 到（> 0）。裸 cargo build 产物该值为 0。

### 🔴 发布物**绝不能**含本地数据与密钥（每次发版必查）

运行数据在 `%APPDATA%\SynaRoute\{config.json, secrets.enc}`，日志在 exe 同级 `logs\`。它们**任一进包**的后果：

- `config.json` 带全部 Key 的地址/映射/余额配置；`secrets.enc` 是密钥库 → 等于**把开发者自己的付费 API Key 发给每个用户**，而且 `secrets.enc` 由 DPAPI 绑账户、到了用户机器解不开，还会顶掉他自己的密钥库；
- `logs\` 里有请求快照（开了日志开关时含对话正文）；
- `~/.tauri/synaroute.key`（更新签名私钥，**无口令保护**）泄露 → 任何人都能签出「验签通过」的伪造更新，而已发布客户端里嵌着公钥、**换钥等于所有老用户自动更新失效**。

**判据是机械检查，不是「我记得没加」**：

```bash
npm run audit:release
```

它查四项：① `git ls-files` 不含运行数据/明文密钥（`secrets/*.key.gpg` 是刻意入库的 GPG 密文，见 `secrets/README.md`）；② `bundle.resources`/`externalBin` 无仓库外引用；③ `dist/` 无数据文件、无密钥内容、无演示数据集（生产必须走 `mockData.prod.ts` 空桩）；④ **当次版本**的产物二进制里搜不到私钥/真实密钥/密钥库内容。

两个已踩过的判据坑（脚本注释里也记着）：**别用 i18n 占位文案当演示数据的判据**（`厂商1（官方直连）` 是输入框灰字提示，每个包都有，会对干净的包报假警）；**别查开发机用户名**（Rust 把 `.cargo/registry` 源码路径嵌进二进制供 panic 用，每个 Rust 程序都有，不含用户数据）。签名密钥只经 `TAURI_SIGNING_PRIVATE_KEY` 环境变量传入，永不落仓库。

完整流程、验证判据、部署步骤见 [docs/04-构建部署指南.md](docs/04-构建部署指南.md)。

## ⚠️ MSIX AppData 虚拟化陷阱（本项目最大惨案，务必先读）

**Claude 桌面应用是 MSIX 打包**（包家族 `Claude_pzs8sxrjxfjjc`）。Windows 对包内进程做 AppData 虚拟化：**Claude Code 及其派生的一切子进程**（Bash/powershell/node/它启动的 SynaRoute）读写 `%APPDATA%\SynaRoute\*` 时，被透明重定向到 `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\SynaRoute\*` 的**包内私有副本**。用户双击 exe 无包身份，读写**真实文件**。→ 两个平行宇宙（曾致「用户 UI 4 条 Key vs Claude 处处验证 6 条」多日排查惨案）。

由此的铁律：
1. **诊断/修改用户真实 `%APPDATA%` 数据必须逃逸包身份**：`schtasks /Create ... /TR "F:\...\x.bat"` + `/Run`（计划任务服务启动无包身份）；bat 与产物放非 AppData 盘（F:\、E:\ 不被虚拟化）。Git Bash 下 schtasks 需 `MSYS_NO_PATHCONV=1`（用完 unset，否则破坏 `taskkill //F`）。
2. **交付验证必须让用户亲自双击启动**，用共享的 exe 同级 `logs\`（非虚拟化）里的「启动自检」行核对（该日志记录每实例实际配置路径/keys 数/用户/exe，正为此设，勿删）。Claude 自己启动的实例活在虚拟宇宙，其表现不代表用户。
3. 症状特征（复发速查）：同一 exe 用户开与 Claude 开数据不同；Claude 探针全对但用户仍旧；`%APPDATA%` 目录清单在 Claude 视角与计划任务视角不同。
4. 不要删包容器里的虚拟副本（可能留 tombstone 遮蔽真实文件）。

## 其他硬规则

- 改配置/二进制前必先备份（带日期后缀，可回滚）。
- 禁止把本机路径硬编码进代码；路径一律动态解析（面向通用用户）。
- 运行数据：`%APPDATA%\SynaRoute\{config.json, secrets.enc}`。

## 文档

- [docs/01-需求规格说明书.md](docs/01-需求规格说明书.md)
- [docs/02-技术架构设计文档.md](docs/02-技术架构设计文档.md)
- [docs/03-UIUX设计文档.md](docs/03-UIUX设计文档.md)
- [docs/04-构建部署指南.md](docs/04-构建部署指南.md) ← 构建部署坑点与流程
- [docs/11-MSIX虚拟化踩坑复盘.md](docs/11-MSIX虚拟化踩坑复盘.md) ← 平行宇宙惨案完整复盘与逃逸手段
- [docs/12-CLI用户手册.md](docs/12-CLI用户手册.md) ← 面向试用用户的 Claude Code CLI 接入手册
- [docs/14-交接与待办清单.md](docs/14-交接与待办清单.md) ← **换机接手必读**：P0 待修缺陷 / 待办分级 / 审查边界 / 判据取证方法
- [docs/15-架构评审报告.md](docs/15-架构评审报告.md) ← **2026-08-03 全量架构评审**：20 条问题分级 + 优化方案 +
  效率/易用性清单 + 三批路线图 + **「不建议改动」清单（防重复劳动，动手前先查）**。
  第一批 9 项已实施（连接池/req_log 守卫/免克隆/并发丢计数/前端 selector），第二三批待办
