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
- 🔴 **更新签名私钥曾泄露（2026-08-14 `29b9257`，08-23 发现，08-27 处置完毕）**：
  完整定性、**三把钥两次换钥的台账**、已补的机械判据、以及**四项决策**都在
  [docs/14 第二十节](docs/14-交接与待办清单.md)。三句最要紧的：
  **泄露的不是现役钥**（现役 `7A46ECB8087DE26F` 干净，**未泄露就不要换**）；
  **删掉文件不等于修好了**（门只扫工作区，不扫 git 历史与 fork）；
  **git 历史刻意不清洗** —— 有 **5 个 fork**，fork 网络保留原始 objects，
  重写之后那把钥仍可从 fork 取到，安全收益≈0 而 333 个 hash 全变是确定代价。
  <br>`secrets/synaroute.key.gpg` 已删除，`secrets/README.md` 随之改写成
  「本目录不放任何凭据文件」—— **不要重新生成一份放回来**。
- **仍未做**：真机验证清单（docs/14 第八节 15 条 + **第八之二节 20 条**）、提交与发 Release
  <br>🔴 **八之二是 2026-08-28 那轮改动的专项验证**，与第八节那 15 条性质不同：
  后者是攒了一个月的待验积压，前者是「这一轮动了什么就验什么」。
  `982c6bf` 碰了**两条命门路径** —— 代理监听绑定（改双栈，判错则本机三端集体 401）
  与令牌取用链（设置页现在是完整令牌的**唯一**出口，日志里只剩指纹）。
  这两处的单测再多也证明不了真实客户端仍然能连，**发版前必须真机走一遍**。
- **大脑聚合多模态 + 工具调用已实现**（2026-08-01，FR-026 / FR-027，详见 docs/14 第一节末专节）：
  参与者可用一组**只读**工具（`read_file`/`grep`/`list_dir`/`codegraph_query`）按需检索，
  开关默认**关**（每轮重发完整历史，额度消耗显著更高）；MCP `images` 参数支持传图
  （相对 cwd、≤4 张、单张 ≤5MB、png/jpg/gif/webp）。
  **三道路径防线恒定生效**：拒 `..`/绝对路径 → canonicalize 后须仍在工作目录内 →
  凭据类文件一律拒读（按「模型给的名字」与「解析链接后的真实落点」**各判一次**，两次都不能省）。
  每条防线都做过故障注入验证（去掉后测试必须变红）。当前基线 **433 passed / 0 failed**
  <br>📌 **基线口径（2026-08-30 实测）**：上面各条历史行里的 311/312/368/383/433/506/719/760/806/911
  都是**当时**的数字，勿当现值。当前实测 `cargo test --lib` = **941 passed / 0 failed / 6 ignored**
  （`cargo clippy --lib --all-targets -- -D warnings` 零警告；`tsc --noEmit` 干净；
  `npm test` **143 passed / 20 文件**；`npm run gates` 全绿，**零棘轮抬高**，
  且 `lib.rs` 2076→2074、`proxy.rs` 3105→3103 两项**下调**）。
  🔴 **B4 落地后（同日）**：`cargo test --lib` = **953 passed / 0 failed / 6 ignored**，
  `lib.rs` 基线进一步下调到 **1907**（余额查询实现搬进 `balance_gate`）。
  接手时请自己跑一遍取当前值，不要引用本文档里的历史数字当基线。
  <br>⚠️ 观察中的一次偶发：`upstream_retry_after_is_propagated_downstream` 在 2026-08-23
  的一次全量跑里红过一次，此后连跑 9 次未复现，单独跑也稳定绿。尚未定性
  （疑似并发下的端口/时序），若再出现请按「测并发缺陷的用例必须并发地写」那条思路查。
- **docs/15 第二三批已实施完 + 第二轮对抗式审查已闭环（2026-08-04）**：详见
  [docs/15 第七节及「七之二」](docs/15-架构评审报告.md)。本轮修掉一条 **P1 数据丢失**
  （Replace 导入删旧密钥却只备份 config.json → 用户回滚配置后密钥永久没了）与两条
  **切分类串台**（在途 IPC 结果写进新分类页，其中桌面端那条因轮询已停表而**永不自愈**）。
  <br>⚠️ **新记一类静默失效**：Tailwind 透明度修饰符只认标度内的值（3.4 是 5 的倍数），
  写 `bg-warning/8` 不报错、**直接一条 CSS 都不生成**。全仓 20 处这么写（含 `Badge`
  除 neutral 外的全部彩色变体），一直是「有边框有字色、独独没底色」而无人发现。
  已在 `tailwind.config.js` 补进 8 / 12 两档（不是把类名改成 /10，要保住设计稿原值）。
  判据：`npm run build` 后 `dist/assets/*.css` 里能搜到 `.bg-warning\/8`。
  <br>🔴 **勘误（2026-08-30 复核）**：这里原先写「**刻意未做**（别当遗漏）：P2-1 `upstream/`
  目录化、P2-7 `service.rs` 抽出、P2-8 `CategoryType` 查表化、`AppSettings` 拆
  `UserPrefs`/`RuntimeState`」—— 那句写于 08-04，而 **docs/15 第三批已在 08-06 把四项全部补做**
  （标题原文「含原先『刻意不做』的四项」）。逐条代码取证：`src-tauri/src/upstream/` 是目录、
  18 个文件（`upstream.rs` 单文件已不存在）；`service.rs` 在（棘轮冻 1443）；
  `model.rs:34` 有 `CategoryMeta` + `:97` 的 `meta()` 唯一穷举 match；`model.rs:1910` 有
  `UserPrefs`（**刻意偏离原方案**：只收窄入参类型，没物理拆 struct）。
  留着那句话的代价是具体的 —— 谁照它动手，会去做四件已经做完的重构。
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
- **余额查询与图标预设已按 cc-switch 取证对齐（2026-08-22）**：判据直接读用户本机
  `~/.cc-switch/cc-switch.db`，不是文档推测。两条硬结论：
  - **它的余额查询不是固定候选链，而是每个供应商一段可编辑的 JS `usage_script`**
    （存 `providers.meta`）。用户那两个站（`Sub2API` = sub.100xlabs.space、
    `「林夕」公益站` = k40.shengqainbang.cn）配的端点都是 **`{{baseUrl}}/v1/usage`**。
    我们此前只抄了它 extractor 的 `??` 字段链、**没抄端点** —— 兜底打
    `/v1/dashboard/billing/subscription`，而 NewAPI 系在那里返回 `hard_limit_usd`
    （配额上限，恒 10000）。**这就是「su2api 全查回 10000 USD」的根因**，
    与「取值路径」无关，怎么调都没用。已改为三条按序探测 + 命中即写回
    （`resolved_url_template` → `Store::set_balance_query_url`），
    只在「用户没填 url 且域名认不出」时探测。
    <br>**刻意没做**：照搬那套用户可编辑 JS（要引入 boa/quickjs + 沙箱 + 超时，
    而绝大多数用户不会写 JS）。若日后真要做，取证已在这里。
  - `icon`/`icon_color` **只有 3 条内置官方模板有值**（20 条 provider 里），
    16 个中转站全是 `None` —— cc-switch 也没给中转站图标，它只是有那个入口。
    三个官方色已对齐实测值（anthropic `#D4915D`、openai `#00A67E`、gemini `#4285F4`），
    挑选器抽成 `BrandPresetPicker` 供厂商页与 Key 编辑器共用（Key 的图标来自其厂商，
    `ProviderKey` 刻意不加平行的 icon 字段）。
  <br>📎 顺带记下：它还有 `provider_endpoints`（多候选 base_url）、`model_pricing`
  （按模型单价，我们是按 Key 倍率）、`usage_daily_rollups`（按天×模型×provider 的用量）、
  `provider_health`、`proxy_request_logs` 等表 —— 这些**我们没有对应实现**，
  想扩功能时那是现成的参照。
- **最大单次输出：「表落后于新模型」已按机制治根（2026-08-22）**。上次只做了「不用用户手填」，
  但内置表止于 Claude 4-5 系 / 各家老版本，于是**每出一个新模型就静默掉到全局兜底 8192**：
  实测 `claude-opus-5`/`sonnet-5`/`fable-5`（官方 128k）、`glm-6`（glm-5 是 96k）、
  `deepseek-v5`、`qwen4-coder`、`gpt-6`（gpt-5 是 128k）全中 —— 用户只看到「回答莫名断了」。
  <br>两条机制 + 两条**机械校验的不变量**（靠注释提醒靠不住，上一轮就是这么漏的）：
  - Claude 侧 `claude_generation_floor`：按「同档位、世代不晚于它」取最大。
    **必须带档位** —— 跨档位会让 `claude-haiku-4-9` 拿到 opus 的 128k（haiku 只有 64k）→ 上游 400。
  - 非 Claude 侧：**家族兜底行（裸家族名）的语义 = 该家族当前已知最好值**，不是最老值。
    真的更低的现役老型号显式列在它之前（`find` 取首个命中）。
  - `family_default_is_at_least_family_max`：兜底行 ≥ 家族内任何具体行。加新行忘了同步就红。
    ⚠️ 判据是「**不含数字**的裸家族名」，不能只看「是否为另一条的前缀」——
    `glm-4` 是 `glm-4-6` 的前缀但它是具体版本，当成兜底行会报假警（写测试时踩过）。
  - `more_specific_rows_come_first_in_both_tables`：顺序反了**不报错、值只是悄悄变小**
    （`claude-opus-4-5` 排到 `claude-opus-4` 之后 → 64k 永远拿不到）。
  <br>**数值取证**：Claude 5 全族与 4.6 之后各代 = 128k（platform.claude.com 各 what's-new 页 +
  AWS Bedrock model card + Vertex AI 合作模型页；4.6 公告原文「double the previous 64K limit」
  同时反证 4.5 系 64k 是对的）。
  <br>**并反转了一条旧决策**：旧注释写「拿不准取家族下限档」（怕填大了 400），前提是
  「同族片段总能命中」，而已实测它不能。两种错法代价不对称 —— 填小 = 静默截断查不到原因；
  填大 = 上游 400，而那句 `max_tokens: X > Y, which is the maximum allowed` **自带正确答案**。
  按「静默错比响亮错更糟」宁可偏大。
- **THINKING_SIGNATURE_INVALID 不是我们的 bug**（2026-08-22 逐处核对）：同协议直通只改顶层字段、
  跨协议只映射 `thinking.budget_tokens` ↔ `reasoning.effort`、响应侧**从不伪造** thinking 块
  （全仓生产代码无一处构造 `"type":"thinking"`）、`budget.rs` 只读不写。真实成因是**故障转移的
  固有代价**：思考块的签名由签发它的那个上游账号签，换 Key 后新上游验不了旧上游的名。
  已修的是呈现：400 不再报成「全部 Key 不可用」（那会把人送去查密钥/额度），
  并附三条可行动出路 + 一句「SynaRoute 不改写思考块」免得反复怀疑代理。
  <br>**刻意没做**：命中该错误就中断故障转移 —— 另一条候选可能**正是**签发那些块的 Key。
  <br>另：`status_code=502, upstream HTTP 400:` 这个格式**不是 SynaRoute 也不是 cc-switch 输出的**
  （前者全仓搜不到，后者格式是 `上游错误 (429): {...}`，已读其 proxy_request_logs 11869 行核对），
  那是中转站自己网关的日志行。
- 🔴 **局域网暴露的额度白嫖已堵（2026-08-27，docs/14 §20.8 唯一的安全级缺陷）**：
  `lan_exposure` 一开就绑 `0.0.0.0`，而转发路径把下游的 `authorization`/`x-api-key`
  **原样剥掉**再换上真实 Key —— 剥之前**从不校验**。于是同网段任何人打
  `http://<内网IP>:47100/v1/messages` 就能拿用户的付费 Key 跑一趟，无需任何凭据。
  实现在 [`lan_guard.rs`](src-tauri/src/lan_guard.rs)（挂在 `proxy` 模块下）。
  <br>**判据：只对非 loopback 强制令牌。** 本机放行不是省事 —— 要求本机也带令牌就得同步改
  **三份**客户端配置（CLI/桌面端/Codex），漏一份的表现是「接入完成但一直 401」，
  用户无从判断是配置还是代理坏了。
  <br>**令牌存 `secrets.enc` 不存 `config.json`**：① `AppSettings` 的字段会被前端批量
  `saveSettings` 覆盖（白名单漏键 → `#[serde(default)]` 补空串），而在这里失效方向是
  「鉴权静默失效」；② 主口令锁定时 `SecretStore::get` 返 `Err`，本模块把 Err 当拒绝，
  于是锁定态**自动 fail closed**，不用多写一行。
  <br>令牌在 `ProxyManager::start` 开局域网时就生成并落一条事件；有测试同时钉住
  「生成」与「可见」：只生成不落事件的表现是「怎么配都 401 而日志里什么都没有」。
  <br>🔴 **勘误（2026-08-27）**：这里原先写「落一条**带明文令牌**的事件……事件仍保留，
  它是**跨时间**的那份留存」。那个设计是**真实泄露**，已改为只落**指纹（前 8 位）**。
  明文进事件等于同时进了三个**用户会分享出去**的地方：① 诊断报告
  （`diagnostics.rs` 取最近 200 条 detail 原样入报告，而出口那道 `redact_config_secrets`
  只认**键名形态**与 `sk-` 前缀 —— 中文句子里的裸十六进制串一个字符都不掩，实测确认；
  更糟的是报告开头声明「本文件**不**包含：任何 API 密钥明文」）；
  ② `logs/*.jsonl`（`append_event_full` 先写文件再进内存环 —— exe 同级、非虚拟化、
  留 30 天、用户会直接 tail 并贴出来，**这一半比报告更大**）；③ 日志页截图。
  拿到任意一份的人只要能进同一网段，就能用用户的付费 Key。
  <br>「跨时间留存」这个理由在 B5（设置页可看/复制/重生成）落地之后已经不成立 ——
  **`LanSection.tsx` 现在是完整令牌的唯一出口**，删掉它用户就再也拿不到令牌。
  <br>⚠️ **改这个的时候漏了一处、被自己的新判据抓住**：401 响应体**仍写着**
  「令牌在应用的日志页里（搜「接入令牌」）」，而日志里已经只有指纹 ——
  照着去搜只能找到一个用不了的短串。这正是本仓「指错方向的提示比没有提示更糟」那条。
  已改为指向设置页，并把判据从「提到令牌」升级成「**指对地方**」
  （`body.contains("设置") && !body.contains("日志页")`）。
  <br>**教训**：摘掉一个信息源时，必须把**所有指向它的指路文案**一起改 ——
  漏掉的表现不是报错，是把用户送去一个空房间。
  <br>🔴 **11 条测试里只有一条能抓住最要紧的那个注入**：把 `accept` 的 `peer` 丢回 `_`
  并给 guard 传硬编码 loopback（= 缺陷本体，所有来源都被当本机），**其余 10 条全绿**。
  抓住它的是源码级判据 `accept_must_pass_the_real_peer_into_the_guard`。
  同 `route_meta` 那条教训：**覆盖了判定函数与分发函数 ≠ 覆盖了两者之间的接线**。
  四条注入（接线退回 / verdict 恒放行 / 无令牌退化为放行 / 不落事件）均验证变红。
  <br>**零棘轮抬高**（7 个相关文件余量全是 0）：把 `service_fn` 那 3 行换成 1 行
  `lan_guard::guarded(...)` 调用、并把只剩测试在用的 `service_fn` import 移进测试段，
  省出的行数正好抵掉 `#[path]` 挂载（写成单行）与 `ensure_token` 那一行。
  <br>**本机判定改走 `is_loopback_peer`（2026-08-28 评审）**：`std` 的
  `Ipv6Addr::is_loopback()` **只认 `::1`**，对 IPv4-mapped 形态 `::ffff:127.0.0.1`
  返回 **false**。当前绑 `0.0.0.0`（纯 IPv4）故生产路径碰不到，这层是为**双栈**准备的
  —— 见下一条：`::` 一开，本机连接就以 mapped 形态到达，裸判会让三端客户端集体 401，
  而那正是本机豁免存在的全部理由。失效方向是拒绝（不是洞），但会制造一个极难归因的支持案例。
  <br>⚠️ **归一只能用 `to_ipv4_mapped()`，不能用 `to_ipv4()`**：后者连
  IPv4-**compatible**（`::a.b.c.d`，已废弃）也转，而 `::1` 正落在那个形态里 ——
  `Ipv6Addr::LOCALHOST.to_ipv4()` == `Some(0.0.0.1)`，一个**非** loopback 的 v4 地址。
  于是「修 mapped」这个改动会把 `::1` 反过来判成非本机，制造出它本要消除的故障。
  有一条测试同时钉住这个坑与它的成因。
  <br>🔴 **绝不能把 `::ffff:` 整段当本机**：里面全是真实 IPv4 地址，
  `::ffff:192.168.1.5` 会绕过整个鉴权。这是本改动唯一可能引入的**安全**方向失效，单独一条判据。
  <br>**被拒次数与事件分开**：事件仍按 IP 去重（防扫描器刷满 `MAX_EVENTS` 环），
  但新增**不去重**的 `denied_count()` 进诊断报告。原实现只有前者，于是一个真在试探的攻击者
  **只留下一条记录然后永远沉默** —— 打 1 次和打 50 万次长得一模一样，
  而「局域网里有人反复撞令牌」恰恰是这个模块唯一想让人看见的信号。
  `SEEN` 集合同时加了 1024 个 IP 的上限（IPv6 下单机可轮换近乎无限源地址，
  而它是进程级只增不减的 `HashSet`）。
  <br>⚠️ **我自己写的计数测试第一版是 flaky 的**：`DENIED_TOTAL` 是进程级 static，
  而「记下 before → 打 3 次 → 断言差值恰好 3」会被同进程另一条**也会被拒**的用例
  （走真 HTTP 那条）插进来打破 —— `cargo test --lib lan_guard` 连跑 3 次**红了 2 次**，
  单独跑永远绿。已加测试段专用的 `DENY_COUNTER_LOCK` 串行化，连跑 5 次全绿。
  同 CLAUDE.md 里「`open_failed_line_count` 并进 `log_dropped_count` 会让
  `flush_logs_drains_the_queue` 当场变红」那条：**进程级计数器的断言必须串行化。**
- **代理监听改双栈（2026-08-28 评审）**：此前只绑 `0.0.0.0`/`127.0.0.1`，那是**纯 IPv4**
  socket，于是 **IPv6 客户端压根连不上** —— 表现是「同一个局域网里有的机器能连、
  有的报 connection refused」，而用户看不出区别在于那台机器走的是 IPv6。
  实现在 [`proxy_listen.rs`](src-tauri/src/proxy_listen.rs)（`#[path]` 挂 `proxy` 下，
  理由同 `lan_guard`/`log_rotate`：`proxy.rs` 余量为 0）。
  <br>**v4 是判据、v6 是附赠**：只有 v4 成功才算端口可用。反过来（v6 成功就接受）在
  Windows 上会造成真实错路由 —— 别人的进程占着 `0.0.0.0:P`、我们的 `[::]:P` 却绑上了，
  客户端连 `127.0.0.1:P` 落进那个**别人的**进程，而我们显示「已启动」。
  v6 绑不上一律静默（容器/精简系统里 IPv6 常被整个停掉，发 warn 对那些用户毫无行动价值）。
  <br>🔴 **`set_only_v6(true)` 必须显式设，别依赖平台默认值**：Windows/macOS 默认 1
  （`::` 只收 v6），**Linux 默认 0**（`::` 双栈通吃）→ 先绑 `0.0.0.0:P` 再绑 `[::]:P`
  会 `EADDRINUSE`。不设的话代码在 Windows 上跑得好好的，到 Linux 上 v6 绑定**静默失败**、
  退回 v4-only —— 也就是本模块要修的缺陷原样复发，且只在一个平台上。
  为此加了 `socket2` 直接依赖（**本就在依赖树里、只有一个版本**，零新增 crate，
  同 `libc`/`security-framework-sys` 的取舍）。
  <br>🔴 **两条「测试自己给自己留逃生口」的教训，都是注入实测抓出来的**：
  <br>① v6 用例第一版写成 `match &b.v6 { Some(..) => assert!(..), None => eprintln!("跳过") }`
  —— 那个 `None` 分支是逃生口：把 `bind_v6` 整个删掉（= 双栈功能没了，缺陷本体），
  **10 条用例无一变红**。已改为先自己绑一次 `::1` 独立探明本机有没有 IPv6
  （`ipv6_available()`），**能绑上就必须要求被测代码也绑上**。改完注入即红 4 条。
  <br>② `only_v6_must_be_set_explicitly` 用了 `production_slice`，而**模块注释里就写着**
  「为什么必须显式 `set_only_v6(true)`」→ 把那行代码删掉判据**照样绿**，注释替代码
  满足了断言。这是本仓第**三**次栽在同一个坑（前两次：`data-dir-env-name-must-match`
  命中自己注释里「❌ 已证伪的修法」；`userPrefsParity` 裸 grep `...rest` 命中
  `prefs.ts` 里**警告不要用 `...rest`** 的句子）。
  <br>已加 `custom_headers::production_code_only`（生产段再剥 `//` 系注释）作单一事实来源，
  并写了一次性脚本把**全部 19 条**源码级判据的字面量与被扫文件的注释交叉比对 ——
  只有这一条被污染，其余 18 条干净。**「代码里必须/不许出现某字面量」的判据一律用
  `production_code_only`，不要用 `production_slice`。**
  <br>**零棘轮抬高，`proxy.rs` 反而 3126→3105**：端口扫描那 20 行收进新模块，
  `SocketAddr`/`TcpListener` 两个 import 只剩测试在用（移进测试段）。基线已下调。
- **日志体积上限已实现（2026-08-27，docs/14 §21.1 B2，用户定调「滚动切分」）**：
  此前只有「按天滚动 + 保留 30 天」，两条管的都是**留几天**，**没有任何东西管一天多大**
  —— 用户一开 `logDownstreamRawEnabled` 一天几个 GB 完全可能，而盘满会连带把
  `config.json`/`secrets.enc` 的原子写搞挂，表现成一堆看不出关联的功能故障。
  实现在 [`log_rotate.rs`](src-tauri/src/log_rotate.rs)（`#[path]` 挂 `store.rs` 下，
  余量 0；改完 `store.rs` 反而 2956→**2938**，棘轮基线已下调）。
  <br>**两级，缺一级都不成立**：单文件 16 MB 滚 `.1.jsonl`/`.2.jsonl`；单日 16 个文件
  （256 MB）封顶，越界删**当天最旧**的。只做滚动 = 磁盘占用一分不少；
  只做「写满即停」= 日志恰好在排障最需要时消失。保最近是因为**开原始日志的人正在排障**。
  第 0 个文件**刻意不带序号**（用户会 tail 它、冒烟脚本遍历它、docs 里到处是这个名字）。
  <br>🔴 **加滚动就必须同时改清理判据，那是一件事**：`cleanup_old_logs_in` 原先
  `strip_suffix(".jsonl")` 再按 `%Y-%m-%d` 解析，而 `"2026-08-27.1"` 解析**失败**
  → 分片**永不被清理**。保留期看着在工作、实际只管住每天第一个文件，
  而磁盘占用这个方向**永不自愈** —— 正是本功能要解决的问题本身。判据收在 `parse_name`。
  <br>🔴 **8 条注入里只有源码级那条抓住了最要紧的**：把写线程改回裸 `write_all`
  （= 上限完全不生效），其余 10 条用例因为都直接构造 `OpenLog` 而**照样全绿**。
  同 `route_meta` / `lan_guard` 的 peer / `mcp::handle_http` 那三次：
  **单元覆盖了组件 ≠ 覆盖了调用它的那条线**，漏掉接线的表现恰恰是静默的。
  <br>另有一条 `#[ignore]` 端到端实测（真写 16 MB，同 `perf_probe.rs` 的模式）：
  源码级判据只证明「调用点在」，它证明「跑起来真的会滚」。实测第 0 个精确停在 16 MB。
  <br>**补修两处（2026-08-27 评审）**：
  ① **滚不动时会每写一行扫一次目录** —— `open` 新分片失败（权限/盘满/杀软锁文件）后
  `size` 仍 ≥ 上限且 `idx` 未变，于是下一次 `write_line` 又进 `roll()` → `existing_indices()`
  → `read_dir`。开着请求日志的转发热路径上那是**每条日志一次目录扫描**，
  而触发条件（盘满）**正是本模块存在的理由** —— 不修的话它会在要解决的那个场景里变成性能悬崖。
  已收成 `give_up_rolling`（`size` 归零 + 告警只发一次）。
  ② **第二条丢日志路径不计数**：写线程 `OpenLog::open` 失败时 `open = None; continue`，
  那一行**无声消失**，而 `log_dropped` 只计队列满。危害在诊断报告只打一个数字、
  用户与排障者都拿它当「是否丢过日志」的答案 —— 盘满时它读 0 而日志正在丢。
  已加 `note_open_failed_line` / `open_failed_line_count`，报告**单独打一行**。
  <br>🔴 **刻意不相加**：两条成因与处置都不同（写得太慢 → 等等看；压根写不了 → 看磁盘和权限），
  合成一个数字会让排障拿不到方向。还有个务实理由 —— 后者是**进程级 static**
  （写线程拿不到 `&Store`），并进 `log_dropped_count` 会让「本 Store 没丢过日志」这类断言
  被同进程其它测试污染：`flush_logs_drains_the_queue` 当场就红了（第一版实际踩到）。
- **`UserPrefs` ↔ `pickPrefs` 补上机械判据（2026-08-27 评审）**：
  [tests/userPrefsParity.test.ts](tests/userPrefsParity.test.ts)，从 Rust 源码抽字段名对账。
  <br>🔴 **此前它只是 `model.rs` 里的一句注释**（原文就写着「**判据**：本结构体的字段集
  必须与 `prefs.ts` 的 `pickPrefs` 逐字段对齐」）—— 把一条纪律称作判据却没有任何机械检查，
  正是本仓「判据存在 ≠ 判对了维度」那条教训（第一例是按**文件名**判私钥）。
  而五条跨语言 parity 测试里，**唯独漏了这条已知会以 P0 形态复发的**。
  <br>失效链：Rust 加字段 → 忘了同步 `pickPrefs` → `saveSettings` 缺键 →
  `#[serde(default)]` 补 false → `apply_to` 写回。用户视角「切个主题，刚开的开关就没了」。
  已发生过两次：两个悬浮球开关；`auto_start` 方向相反（把**刚关掉**的自启动装回系统，P0）。
  <br>双向注入均变红：Rust 加字段报出 `someNewToggle`；前端多加 `autoStart` 被独立那条
  「后端自管字段」拦下。
  <br>⚠️ **写它时踩了本仓记录过的假阳性**：裸 grep `...rest` 命中的是 `prefs.ts` 注释里
  **警告不要用 `...rest`** 的那句话。同 `data-dir-env-name-must-match` 第一版命中自己注释里
  「❌ 已证伪的修法」。已改为先剥注释，并加一条 `toContain("export function pickPrefs")`
  防止剥过头变成空洞的绿。**判据说「代码里别这么写」，就只能看代码。**
- **模型锁只增不扫已修（2026-08-27 评审）**：`model_locks` 的条目只在「该模型成功一次」时
  被 `decay_model_lock` 删除（减半到 0），而一条「什么模型都 404」的 Key 上**成功永远不会发生**
  —— 用户每换一个模型名就多一条，全部随 `HealthState` 进 `config.json`，
  而健康态每次落盘都整份序列化。是单调增长的持久化状态，方向上永不自愈。
  <br>已加 `sweep_stale_model_locks`，挂在**唯一会让该表变大的地方**（`record_model_unavailable`
  的写锁内，不额外加一次 read-modify-write）。
  <br>🔴 **判据刻意不是「到期就扫」**：`fail_count` 是退避阶梯的记忆，到期下一秒就扫掉会让
  「每隔几分钟失败一次」的模型永远停在第一档 120s —— 正是 `decay_model_lock` 注释里
  「成功即删」要避免的高频白打上游。阈值取 2× `MODEL_LOCK_MAX_MS`（1 小时）。
  对 `model_lock_active` / `active_model_lock_count` **无语义影响**（两者只看 `until > now`）。
  <br>⚠️ **我自己写的第二条测试声称「判据放宽成到期就删时会一起变红」，注入证明那是假话**
  —— 它照样绿（用例里压根没有「刚到期」的条目）。已补上使该声称成真。
  同「注入不变红时先怀疑用例没压到那个分支」那条。
  <br>**另一条不需要测试**：「扫描必须排在 `entry()` 之前」由**借用检查器**保证 ——
  挪到之后是 E0499，编译不过（实测确认）。已把注释改为归功于编译器，
  而不是假装有测试在守：把硬保证写成软保证是退步。
- **`headers_json` 已接上（2026-08-27，docs/14 §21.1 B1）**：此前是纯后端死字段
  （定义、落盘、诊断会脱敏，但**转发零读取、前端零 UI**）。真实需求存在：
  OpenRouter 要 `HTTP-Referer`/`X-Title`。实现在
  [`custom_headers.rs`](src-tauri/src/custom_headers.rs)（挂 `proxy` 下）。
  <br>⚠️ **旧文档把它写成「Key 编辑器里那个输入框」—— 根本没有输入框**，
  与 `upstreamRetryEnabled` 同一个错法（都是从 docs/14 转述而没核实前端）。
  **判据必须落到代码，转述不算取证。**
  <br>**两道防线方向不同，缺一不可**：保存时**拒绝并说明**（静默忽略会让人以为生效了，
  然后去查中转站/网络）；转发时**静默过滤**（字段在有 UI 之前就存在，用户可能手改过
  `config.json`，而放一个 `authorization` 过去会顶掉真实 Key → 上游 401，
  日志只显示「鉴权失败」，没人会想到是自定义头干的）。合并位置刻意在**鉴权头之前**。
  <br>🔴 **保留字段清单只有一份**：原 `proxy.rs::is_stripped_header` 移成
  `custom_headers::is_reserved`，它同时是「哪些下游头不透传」与「自定义头黑名单」。
  两个用途共用一份 —— 否则加新的代理自有头时只改一处，另一处静默成洞。
  前端那份由 `tests/reservedHeadersParity.test.ts` 从 Rust 源码抽字面量对账。
  <br>🔴 **第 5 次撞同一类接线盲区**（前四：`mcp::handle_http` / `route_meta` /
  `lan_guard` 的 peer / `log_rotate` 的写线程）：14 条校验用例全都直接调函数，
  于是**把字段从 KeyEditor 摘掉、或忘了放进保存 payload，它们照样全绿** ——
  而那就是「界面能填、存不进去」这个缺陷本身。已补三条源码级判据。
  <br>**零棘轮抬高，三个基线反而降**（`proxy.rs` 3137→3126、`KeyEditor.tsx` 1639→1624、
  `i18n.ts` 1500→1475）。腾空间靠抽同类字段成组件 + 清 **14 条死文案**。
  <br>⚠️ **清死文案踩过一个假阳性**：第一版把 `balance.tpl.*` / `settings.health.*` /
  `settings.failoverBudget.*` 这 17 条也报成死键，而它们是 `t(\`前缀.${变量}\`)` 拼出来的，
  删了界面会显示原始 key。正确判据是**先穷举全仓所有动态拼 key 的前缀**（只有 7 种），
  再逐条核实功能是否还在。同「别用 i18n 占位文案当演示数据判据」那一类。
- **局域网令牌的设置页 UI 已做（2026-08-27，docs/14 §21.1 B5）**：补的是**已上线功能的缺口**
  —— 局域网鉴权 v0.1.41 就发了，而在此之前用户拿令牌只能「去日志页搜『接入令牌』」。
  实现在 [`LanSection.tsx`](src/components/LanSection.tsx)（开关 + 令牌面板收在一起）
  + `lan_guard::{read_lan_token_from, regenerate_lan_token_in}` + 两个 IPC 命令。
  <br>🔴 **两条刻意行为**：① **读令牌只读、绝不生成** —— 生成点只有一个
  （开局域网时的 `ensure_token`），若读也生成，「打开设置页」这个纯查看动作会给没开
  局域网的用户凭空造出密钥库条目，且把单一生成点变成两个。
  ② **锁定态返 `Err`，不能退化成 `Ok(None)`** —— 后者在界面上是「还没有令牌」，
  于是用户点「重新生成」，而库里其实**已经有**令牌 → **所有已配好的局域网客户端立刻 401**，
  且用户不知道自己刚做了什么。这是本功能最危险的失效方向，专有一条测试盯着。
  <br>⚠️ **那条测试是补上来的**：原先只有一条用**未锁定**库的用例，于是把
  `Err(_) => Err(..)` 改成 `Err(_) => Ok(None)` **照样全绿** —— 锁定分支压根没被跑到。
  同「注入不变红时先怀疑用例没压到那个分支」那条。6 条注入最终全部变红。
  <br>**零抬高，三个基线反而降**：`lib.rs` 抽出 `usage_commands.rs`（腾 38 行）、
  `mockData.ts` 抽出 `mockData.events.ts`（腾 162 行）、`SettingsPage.tsx` 抽出
  `ToggleRow.tsx`（腾 54 行）。
  <br>⚠️ **抽 `ToggleRow` 顺带断了一个循环依赖**：第一版让 `LanSection` 从
  `SettingsPage` 导入 `ToggleRow`，而 `SettingsPage` 又导入 `LanSection` ——
  React 组件在模块循环里可能在求值时还是 undefined，是会偶发、难查的那类故障。
  <br>⚠️ **`read_lan_token_from` 的 `match` 分支里不许再取锁**：scrutinee 是
  `store.secrets.read()` 的临时 guard，它在整个 match 期间存活，分支里取 `.write()`
  会 RwLock 自锁、当场挂死。写注入验证时踩过，一条测试挂了十分钟才发现
  **不是「注入无效」而是「进程挂死」**。注入脚本因此加了每条超时。
- **Codex 模型目录已实现（2026-08-28）**：让**非 GPT 模型出现在 Codex 自己的模型选择器里**，
  并让「思考强度」档位由我们声明、由 Codex UI 真实呈现。实现在
  [`codex_catalog.rs`](src-tauri/src/tools/codex_catalog.rs)（`#[path]` 挂 `tools/codex.rs` 下，
  理由同 `lan_guard`/`log_rotate`：`codex.rs` 余量只剩 35 行）。
  <br>**官方机制是 `model_catalog_json`**（config.toml 的键，官方文档原文
  "Optional path to a JSON model catalog (applied on startup only)"），文件 schema 就是
  HTTP `/models` 端点那同一个类型（`ModelsManagerConfig.model_catalog: Option<ModelsResponse>`）。
  cc-switch 从 v3.16.0 就走这条（生成 `cc-switch-model-catalog.json`），codexloom / moon-bridge /
  cloudpods aiproxy 也都是。
  <br>🔴 **两条通道互斥，不能都做**：`StaticModelsManager::raw_model_catalog` 把
  `_refresh_strategy` 与 `_http_client_factory` 整个丢弃、`refresh_if_new_etag` 是空实现
  → **配了目录就永不联网**。选本地文件而非改 `/v1/models` 响应的理由：① 那条路会写
  `~/.codex/models_cache.json`，而它是**全局单文件、无 provider 维度**（只有一份
  `fetched_at`/`etag`/`client_version`），「切回官方后会不会读到我们的列表」没有字段能保证；
  ② 要改 `proxy.rs`（余量 0）。代价是改模型列表要重启一次 Codex（同
  `rewrite_registered_clients` 那条已知代价）。
  <br>**顺带解释了此前那个 workaround 的根因**：`active_efforts` 的注释写「Codex Desktop 对
  自定义 provider 不下发 `reasoning.effort`」—— 因为 Codex 只对
  `supported_reasoning_levels` 非空的模型显示并下发档位，而未知模型走 fallback metadata、
  那个字段是 `[]`。声明了档位，UI 就变成真的，`active_efforts` 退回成兜底。
  <br>**四条实测判据**（codex-cli 0.150.0-alpha.8，隔离 `CODEX_HOME` + `codex debug models`）：
  `base_instructions` 或 `model_messages.instructions_template` **至少有一个**（都省 →
  **Codex 启动即报错、完全起不来**）；目录是 **full replacement**（只放我们 2 条 → 官方 10 条
  全消失）；非 GPT 名字**不被过滤**（`claude-opus-4-8` 原样保留，与 Claude 桌面端那个 `sD()`
  完全不同，**不需要** `claude-synaroute-` 前缀）；`minimal_client_version` 是**字符串**
  （官方单测里那个数组 `[0,99,0]` 已过时）。官方 `priority` 占 **1~43**，故我们从 50 起。
  <br>🔴 **`supported_in_api` 必须为 `true`**：`ModelPreset::filter_by_auth` 是
  `chatgpt_mode || supported_in_api`，而我们走 `experimental_bearer_token` →
  `chatgpt_mode = false`。给 false 的表现是模型**静默从选择器里消失**。
  <br>🔴 **档位按 `ProviderKey.protocol` 推导，不猜模型名**：Anthropic 上游 →
  `convert.rs` 自己算 `thinking.budget_tokens`（**生效由我们保证**）；原生 Responses → 原样透传；
  **Chat Completions → 不声明**。cc-switch 用 22 家预设换来的结论：只有「思考开关」的供应商
  （Kimi/GLM/Qwen/MiniMax/MiMo/SiliconFlow）**在 Codex 里调档位不会有任何效果**。
  口径是**交集**（一条 Chat 备用 Key 就让整组不声明）—— 同 `models_for_apply` 那条
  「超集口径 → 故障转移后必然 404」。
  <br>**声明的档位只有 `low/medium/high/xhigh`**，与官方 `gpt-5.5` 一字不差。**不含 `max`/`ultra`**：
  `effort_to_thinking_budget` 对它们走 `_ => return None` = 不开思考，声明它等于
  「用户选了最高档反而完全不思考」，方向最坏的界面撒谎。有源码级判据钉住
  （用 `production_code_only` 剥注释 —— 本仓已三次栽在「注释里的字面量满足了断言」上）。
  <br>**`tool_mode` 把 docs/13 那 900 条统计对上了官方名字**：官方基底实测
  `gpt-5.6-*` = `"code_mode_only"`（exec 沙箱那 241 条）、`gpt-5.5`/`5.4`/`5.2` = **`null`**
  （顶层 tools 那 73 条）。`claude-opus-4-7` 那 18 条走的就是 `null` 形态，我们一律发 `null`。
  也就是说工具承载形态从「受模型名支配」变成**我们显式声明**。
  <br>🔴 **基线是「Codex 今天对未知模型用的那份 fallback metadata」，不是官方 GPT 条目**。
  逐字段对着 `model_info_from_slug` 抄，**只改四项**（`visibility` none→list、
  档位 []→四档、`priority` 99→50+i、`context_window` 恒 272000→有取证时用真实值）。
  **刻意没改**：`apply_patch_tool_type` 保持 `null`（fallback 就是 None，也就是说非 GPT 模型
  今天**没有** apply_patch 工具、一直用 shell 改文件；给 `"freeform"` 是新增一个未验证的工具，
  而本轮判据是「不退化」不是「顺手增强」）、`include_skills/plugin/apps_usage_instructions`
  全 `false`（⚠️ `include_apps_usage_instructions` 在 `ModelInfo` 里是
  `#[serde(default = "default_true")]`，**省略它会变成 true**，那就悄悄开了一个 fallback 下
  关着的开关）、`truncation_policy` 用 **bytes** 不是 tokens（官方 GPT 条目用 tokens，
  fallback 用 `bytes(10_000)`，抄错这一个键会改变工具输出的截断行为）。
  <br>🔴 **顶层 `model` 改成「已有值且仍可服务就不覆盖」**：`get_default_model` 开头是
  `if let Some(model) = model { return model }` —— **config.toml 的 `model` 一旦有值就赢**，
  目录里的 `isDefault` 根本不参与，而 Codex 会把用户在 `/model` 里的选择写回这个键。
  旧实现无条件写，于是每次接入（改端口/Key 变动都会触发重写）都把用户刚选的模型冲掉。
  cc-switch 修过同一条。第三支是「已有值但不在列表里 → 换成首个」，不换的话 Codex 会拿一个
  我们服务不了的名字去请求。
  <br>**空模型列表既不写目录也不写指针**：`{"models":[]}` 会让 Codex 对着空列表挑默认模型
  （行为未验证），而用户在菜单里一个都选不到 —— 比回落官方条目糟得多。真实成因是
  「无启用 Key」或「多 Key 交集为空」。
  <br>**不认领用户自己的 `model_catalog_json`**：判据只有文件名（`pointer_is_ours`）。
  cc-switch 为此修过一条（#6087）—— 它早期版本无条件把指针指向自己的文件、丢弃用户路径，
  而且**改过的指针不会被还原**，只能发公告让用户手动指回去。
  <br>**漂移新增一支 `CatalogFileMissing`，优先于所有 provider 形态**：不单独判会落进
  `OurTablePointsElsewhere`，那句告警说「我们的表指向别处」并把**我们自己的**端点回显给用户
  —— 而真实情况是 Codex 一启动就报 `failed to parse model_catalog_json` 然后退出。
  「指错方向的告警比没有告警更糟」那条。
  <br>🔴 **系统提示词内嵌了官方 `prompt.md` 逐字节副本**（20903 字节，Apache-2.0，
  声明在 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)）。我第一版写的是 ~1 KB 自撰提示词，
  理由「含 based on GPT-5 的错误陈述 + 许可风险 + 版本漂移」——**前两条是错的**：那句在
  `DEFAULT_PERSONALITY_HEADER` 常量里，`prompt.md` 全文零 `GPT` 字样、模型中性；
  openai/codex 是 Apache-2.0，逐字再分发合法。真正推翻它的是
  `model_info_from_slug`：**今天**没有目录时 Codex 给未知模型填的就是
  `instructions_template: Some(BASE_INSTRUCTIONS)`，而那就是 `prompt.md`，
  里面有 `# Tool Guidelines` / `## Shell commands` / `## update_plan` 三节工具契约。
  换成短的 = **静默削弱现有工具调用能力**，正是本功能要保住的东西。
  <br>⚠️ 我那条「短一点是安全的」推断的破法值得记：旁证是 `codex debug prompt-input`
  ——`input[]` 里那条 9965 字符的 developer 消息由 Codex 自己注入（skills/escalation/sandbox），
  与本常量无关。**那个观察对，但结论跳了一步**：Responses API 的 `instructions` 是
  **顶层字段**，压根不在 `input[]` 里，那条探针从头到尾没看见过本常量。
  用「看不到差异」当「没有差异」，是本仓记过多次的那类假绿。
  <br>**16 条故障注入全部验证变红**，另有一条 `#[ignore]` 端到端用**真实 codex 二进制**
  校验（`catalog_is_accepted_by_the_real_codex_binary`，实测解析出 2 条、顺序正确）：
  源码级判据只证明「调用点在」，它证明「Codex 真的读得懂」。
  <br>⚠️ **探针准备**：Desktop 包内那份 `codex.exe` 在
  `C:\Program Files\WindowsApps\OpenAI.Codex_*\app\resources\` 下带 ACL、**不能直接执行**，
  要先 `cp` 出来（307 MB）。跑法：
  `SYNAROUTE_CODEX_PROBE=<path> cargo test --lib catalog_is_accepted_by_the_real_codex -- --ignored`。
  <br>🔴 **注入脚本自己踩了「恒绿的门」那个坑**：第一版用
  `cargo test --lib <fn_name> -- --exact`，而 `--exact` 要求完整路径，只给函数名匹配到
  **0 个测试**、cargo 返回成功 → **14 条注入全部报「仍绿」**。已改为解析
  `test result: ok. N passed` 并在 `N == 0` 时判脚本失败。同 `invoke-command-must-exist`
  那条门「解析到 0 个就主动判失败」的教训 —— 只是这次栽在验证脚本上。
  <br>⚠️ **两条我自己写松的用例**（都是注入不变红才发现的）：① 判「不报某一支」时写成
  `assert_ne!(state, 那个特定值)`，而注入后返回的是同一变体但 `path` 不同的值 —— 不等、照样绿，
  应该 `matches!` 判变体；② 更根本的一条：那个场景里 `is_intact` 仍为真、直接
  `return Intact`，**压根走不到被注入的那段** —— 必须让 config 因**别的**原因不完好
  （用了端口漂移）才压得到。同「注入不变红时先怀疑用例没压到那个分支」那条。
  <br>**零棘轮抬高**：`tools.rs` 两处都是**一行换一行**（分发点改传参、还原点改函数名），
  目录生成 / 顶层 `model` 决策 / 告警文案全收在新模块（`wire_into`、`missing_catalog_warning`），
  `codex.rs` 净增 16 行（898/900，**余量只剩 2** —— 下次动它必须先腾）。
  当前基线 `cargo test --lib` = **907 passed / 0 failed / 5 ignored**，clippy 干净，
  `npm run gates` 全绿，`tsc --noEmit` 干净。
  <br>📌 **仍未验证（真机才能闭环）**：接入后在 Codex Desktop 里①菜单是否真的列出非 GPT 模型、
  ②档位选择器是否出现且切换生效、③工具与 MCP 是否与今天一样正常（尤其 `tool_search` 那条
  MCP 延迟检索链，docs/13 §11 有取证基线可对照）。
  <br>✅ **①②已真机验证通过（2026-08-28，用户截图为证）**：接入后重开 Codex Desktop，
  模型菜单列出了目录里的 **8 条**，其中 **`5.3 Codex Spark`（`gpt-5.3-codex-spark`）
  在官方基底里根本不存在**（用户日志里就是 `Unknown model … fallback`）——
  它只可能来自我们的目录，这是决定性证据；同时官方 visible 的 `gpt-5.2` **消失了**，
  full replacement 在 UI 层面也确认。右侧面板出现「推理强度：高」，**②档位选择器成立**。
  <br>🔴 **我此前那句「cc-switch 那句『Codex app 不支持多模型选择』已过时」——
  结论是对的，但当时的依据（asar 静态取证）不足以支撑它，中途一度撤回过。
  现在有实测了。** 教训不是「猜对了就行」，而是：**没有第三方印证时，
  静态取证只能提出假设，不能下结论** —— 那句话该等到今天才写。
  <br>⚠️ **一处偏差，尚未定性**：目录 9 条里 **`codex-auto-review` 没有出现在菜单**。
  我们给它 `visibility: "list"`，而官方基底给的是 `"hide"`。说明 **Desktop 前端在
  `visibility` 之外还有一层过滤**（疑似按 slug 硬编码排除 auto-review 这类内部模型）。
  对本功能无害（那本来就不该让用户选），但它意味着**「声明 list 就一定显示」不成立**，
  日后若发现某个模型莫名不显示，先查这一层。
  <br>③（工具与 MCP）仍未验 —— 那要代理在跑 + 真发一轮请求。
  <br>📌 顺带在真实数据上印证了 D4 那条修复：用户原本 `model = "gpt-5.3-codex-spark"`，
  它在可服务列表里 → `wire_into` **保留了它**，没改成列表首个。
  <br>📌 另一个观察：Desktop 把 slug `gpt-5.3-codex-spark` 显示成 **「5.3 Codex Spark」**，
  而我们给的 `display_name` 就是 slug 原文 —— 说明**前端对显示名做了美化**（去 `gpt-` 前缀、
  连字符转空格、词首大写），`display_name` 可能没被直接采用。非 GPT 名字会被显示成什么样
  尚未观察（`claude-opus-4-8` → ？），不影响功能但会影响用户能否一眼认出。
  <br>🔴 **收尾时抓出一条自己漏掉的、会让功能一半失效的缺陷**：`proxy.rs` 那行
  `active_model_of(category).unwrap_or(client_model)` 是**无条件覆盖** —— 用户在 Codex 菜单里
  选了模型、Codex 如实发过来，代理又把它换成应用内选的那个。「选择器做出来了、选了不算」，
  而且静默（日志里显示的是覆盖后的名字）。计划表 D4 里列了这条，实现时只做了
  `config.toml` 顶层 `model` 那一半 —— **那是 Codex 启动时读的默认值，和转发时的覆盖是两条
  独立路径**。已收进 [`model_choice.rs`](src-tauri/src/model_choice.rs)（挂 `proxy` 下）。
  <br>**判据按分类分支，因为两类客户端发模型名的动机根本不同**：Codex 是**用户在菜单里选**
  → 可服务就尊重、`active_models` 退成兜底；CLI/桌面端是**客户端按任务自动发**
  （haiku 干杂活、opus 干重活，三档映射存在的全部理由）→ 保持强制语义。
  在 CLI 那边也改成「可服务就尊重」会让 `active_models` 变成死字段：它发的三档名几乎总是
  可服务的，于是覆盖永不发生、用户设的「全部走 opus」静默失效。
  <br>**对照：`active_efforts` 没有这个问题** —— `inject_default_effort` 里写着
  「已带 effort（无论 Codex 何时开始下发）→ 尊重下游」，写它的人预见到了这一天。
  <br>顺带修掉两条**过时注释**（本仓最在意的那类分叉）：`active_models` 的「某些客户端
  （如 Codex）模型菜单是内置固定清单」已不成立；`active_efforts` 的「Codex Desktop 对自定义
  provider 不下发 effort」已被本轮改变，且它那句「仅在上游非原生 Responses 时注入」
  **早就与代码不符**（`inject_default_effort` 里 `let _ = upstream;`）。
  <br>本条 4 条注入全部变红，含一条**源码级接线判据**（`the_forwarding_path_must_go_through_pick`）
  —— 前四条单测都直接调 `pick`，把 `proxy.rs` 那行改回去它们照样全绿，那正是缺陷本身。
  这是第 7 次撞同一类接线盲区。**`proxy.rs` 反而 3105→3103，基线已下调。**
  <br>⚠️ **两处仍是「界面撒谎」，需要产品决策**：① 托盘「Codex 模型」快切子菜单
  （`tray_model_switch_enabled` 默认开）—— 修完之后它对「Codex 发来可服务名字」的场景无效，
  而接入 catalog 后那是常态，也就是**能点、几乎永远没反应**；② 前端没有任何
  「改了模型列表要重启 Codex」的提示，而 `model_catalog_json` 官方注明
  "applied on startup only"，用户加个 Key 后菜单不变、无从知道原因。
  <br>✅ **上面那两条已闭环（同日）**：
  <br>**①** 应用内选模型（主窗口下拉 + 托盘子菜单）改走
  `tools::codex::select_model` —— 它**同时**写 `config.toml` 的顶层 `model` 与
  `active_models`，两处指向同一个模型，故不会出现「托盘说切到 A、实际走 B」
  （同托盘代理启停那条「起=写、停=还原」的纪律）。
  <br>关键事实：`model` **不是** `model_catalog_json` 那种 "applied on startup only" ——
  `get_default_model` 在每次 session 初始化时读它，**新开一个会话就生效**，不必重启 Codex。
  托盘事件文案已从「即时生效」改成「新会话生效」。
  <br>三条刻意行为：没接入过就只写兜底（不凭空造 `config.toml`）；`model_provider` 不是
  synaroute 就一个字节都不动（用户可能正用 cc-switch 或官方登录）；空串（「跟随客户端透传」）
  **不动** `config.toml`（那个选项的语义就是「不干预」，删掉 `model` 键会连带清掉用户在
  Codex 菜单里的选择）。
  <br>🔴 **我第一版把 category 写死成 Codex，会让 CLI/桌面端的模型下拉静默失效** ——
  `set_active_model` 命令是收 `category_id` 的。抓住它的是 clippy 的
  `unused variable: category_id`，不是任何测试。分派现在收在 `select_model` 里
  （非 Codex 只转发 `set_active_model`），刻意不放 `lib.rs`：两个入口分两处写必然漂移。
  <br>**②** 提示落在**接入成功消息**里（`codex_catalog::apply_note`），**零前端改动** ——
  那条消息同时进「接入结果提示」与事件流（改端口 / Key 变动触发的客户端配置重写也落它），
  是这句话唯一必须落地的地方。`SettingsPage.tsx` / `KeyEditor.tsx` / `i18n.ts` 三个顶格文件
  一行都没动。顺带把 `apply_at` 里那 4 行 `model_note` 收成 1 行，**给 `codex.rs` 腾回 3 行余量**。
  <br>**本轮 9 条注入，7 条按预期变红，另两条各有结论**：
  <br>🔴 「`apply_at` 不再用 `apply_note`」**仍绿 = 真盲区** —— 没有任何测试检查过
  `apply_at` 返回的消息内容，把提示整段拿掉照样全绿。已在
  `apply_wires_the_catalog_pointer_and_the_file_together` 里补 `msg.contains("重启")`。
  <br>「`config.toml` 不存在时凭空造一份」**仍绿 = 判据重叠，不是盲区**：文件不存在时读出空串、
  parse 成空表、`model_provider` 取不到 → `ours` 为假 → 同样早退。两道门重叠已写进注释，
  门**保留**（早退的语义是「没接入过就不碰」，不该依赖另一道门的副作用成立）。
  同 CLAUDE.md 里 `B64_MIN_RUN` 那条「注入不变红先怀疑判据本身重复」。
  <br>⚠️ **源码级判据第一版报了假阳性**：查 `set_active_model(` 命中的是 `lib.rs` 里那个
  **同名 tauri 命令的定义行** `async fn set_active_model(`。已改为查带点的调用形态
  `.set_active_model(`。这是本仓第四次栽在「判据说别这么写、却没只看调用」上。
  <br>⚠️ **全量跑偶发红过一次**，根因不是功能：新加的测试临时目录用 `pid + 纳秒`，
  而本机 `timestamp_nanos` 量化粒度**只有 100ns**，同进程并发的两条用例撞到同一个目录、
  互删对方文件。已加进程内自增序号（同 `ccswitch::db_copy_path` 那条的修法），
  连跑 4 次全绿。
  <br>**棘轮两项下调**：`lib.rs` 2076→2074、`proxy.rs` 3105→3103。
  <br>**收尾审查又修出 5 条（2026-08-28）**，前两条有回归测试 + 注入验证：
  <br>🔴 **切模型抢走了「接入前快照」**：`select_model_at` 原走 `backup_and_write_bytes`
  （首写即锁），于是「接入前」这个时间点被提前到「因为切一次模型而第一次碰 config.toml 之前」
  —— 与 `write_without_locking_snapshot` 文档里记的那次真实事故**完全同类**
  （MCP 注册抢走 `.bak`，还原时把一份陈旧全量配置整份写回）。复现路径：接入过又还原过
  （`.bak` 已删）→ config.toml 里又出现我们的 provider（cc-switch 存的档 / 手改）→ 切一次模型。
  已改走 `write_without_locking_snapshot`：切模型不是接入，也不需要快照（还原时
  `config.toml` 整份从 `.bak` 恢复，`model` 键随之回到接入前的值）。
  <br>🔴 **写 config.toml 与写 `active_models` 的顺序反了**：原先先落兜底口径，于是客户端配置
  写失败（Codex 正占着文件 / 权限不足，真实会发生）时托盘显示已切到 A、Codex 仍用 B ——
  **正是这个函数存在的目的所要消除的那个现象**。已改成「先写客户端配置、成功后才动兜底」。
  <br>`model_choice::pick` 判据用 trim 后的形态、返回却是原串 → 带空白的名字判得出、
  却路由不到（`resolve_model` 拿到的是另一个字符串）。已返回归一后的值。
  <br>两处**注释过度承诺**，按实际收窄：`context_window_across_keys` 说「所有启用 Key 上都成立」
  ——实际只有**填过** `context_window` 的 Key 参与取最小值，没填的 Key 可能更小（取证缺失的
  固有限制，失效方向是上游 400、响亮）；`restore_side_files` 说「都汇总上报」——**不早退**是真的，
  **都上报**只做到「第一个错误 + 已完成项」。
  <br>**全局机械扫查（生产段、剥注释）结论**：`unwrap` 20 处（11 在 `perf_probe` 的
  `#[ignore]` 探针里，其余是「常量头值」或「前面刚判过 `is_none` 就 return」）、`expect` 21 处
  （启动路径失败即崩 + 常量必然合法 + 2 处不变量断言）—— **无缺陷**。
  密钥泄露维度：敏感词 × 输出汇的交集只剩 2 处，都是 `format!("Bearer {secret}")` 构造鉴权头
  （必须的），**没有任何密钥进日志/事件/错误消息的路径**。
  <br>⚠️ **建议级（未改）**：`proxy.rs:1961` / `:2551` 那两处 `expect("…必然已收集")` 依赖
  人工维护的跨语句不变量（`sse_dir` 与 `tool_sets` 是「同一个条件」），破了就 panic 在转发
  热路径上。用类型把两者绑成一个 `Option<(dir, sets)>` 可让编译器保证，但那是重构、
  且 `proxy.rs` 余量为 0。
  <br>🔴 **我那个一次性扫查脚本自己被字面量污染了**：第一版自写「找 `#[cfg(test)] mod tests`」
  的最小实现，而 `codex.rs` 测试段里有一条源码级判据的**字符串字面量**
  `"\n#[cfg(test)]\nmod tests"` —— 切断点被推后 800 行，79 个 `unwrap` 里大半是测试代码的
  假命中。改用项目自己的 `scripts/lib/rust-source.mjs` 后降到 20 处。
  这是本仓第 **5** 次同类（前四：`data-dir-env-name-must-match`、`userPrefsParity`、
  `only_v6_must_be_set_explicitly`、`check-tailwind-tokens`）。**别再自己写第二份测试段判据。**
  当前基线 **907 passed / 0 failed / 5 ignored**（连跑 2 次），累计 **32 条注入**全部有结论；
  `npm test` 133 passed / 17 文件，`tsc --noEmit` 干净。
  <br>**把「建议级」与「只统计没细看」的维度全部做完，又修出两条真缺陷（2026-08-28）**：
  <br>🔴 **局域网令牌的常量时间比较有边界失效**：`lan_guard::constant_time_eq` 第一版写的是
  `let mut diff = (a.len() ^ b.len()) as u8;` —— `usize ^ usize` 再 `as u8` **只保留低 8 位**，
  于是长度差恰好是 **256 的倍数**时长度差异被整个丢掉（`3 ^ 259 == 256`，截断后为 0）。
  此后逐字节比较也全 0（短的那边超出部分取 0），函数返回 **true** ——「真令牌 + 256 个 NUL
  后缀」被判为相等。实际不可利用（HTTP 头值不允许 NUL，且攻击者得先知道真令牌），
  但这是判据在边界上静默失效。已改为 `u8::from(a.len() != b.len())`（仍不提前 return，
  不泄露长度）。⚠️ **原有那条测试的长度差全是个位数，所以它一直是绿的** ——
  新加的 `length_difference_survives_any_multiple_of_256` 里专门断言了「这个 pad 必须真的让
  旧实现截断成 0」，防止用例又压不到边界。
  <br>🔴 **上游一个整数能让我们分配数百 GB**：`sse.rs` 的 Chat 增量累积是
  `while self.tool_calls.len() <= idx { push((String,String,String)) }`，而 `idx` 直接来自
  上游响应的 `tool_calls[].index`。上游发 `{"index": 4294967295}` → 40 亿个三元组
  （每个 ≥72 字节）→ 进程被 OOM 杀掉。**用户接的是第三方中转站，那正是这条链路上不可信的
  一方**，而本仓在别处已对上游做过同类防护（`TAIL_WINDOW_BYTES` / `REQ_LOG_CAP`）。
  已收进 `upstream::tool_slot`（上限 256、越界**拒绝**而非钳制 —— 钳制会把不同 index 的增量
  挤进同一个槽，拼出参数错乱的工具调用，比丢一条增量糟得多；警告只发一次）。
  <br>**同文件另外 4 处 `idx` 不受影响**：`771/804/917/955` 用作 `HashMap`/`HashSet` 的**键**，
  单条目、无放大。判断依据是「用途」不是「来源」。
  <br>**全维度扫查结论**（生产段、剥注释）：`panic!/unreachable!` 3 处（1 处有前置分派、
  2 处在 golden 测试基建里）、`unsafe` 10 处（8 处 Windows 路径 API + 2 处 DPAPI，
  **`CryptProtectData`/`CryptUnprotectData` 的返回值都用 `?` 处理了**，失败时提前返回、
  不会走到 `from_raw_parts` 读野指针 —— 无缺陷）、窄化 `as` 33 处（除上面两条外均有
  `clamp`/`min`/`saturating_*` 守着）、常量下标 13 处（`workdirs.rs` 有 `len() >= 3` 短路、
  `service.rs` 有 `issues.is_empty()` 早退、`tray_icon.rs` 是 RGBA chunk —— 全部安全）。
  <br>🔴 **我这一轮两次栽在自写脚本上，都值得记**：① 一次性扫查脚本自写测试段判据，
  被 `codex.rs` 里那条源码级判据的**字符串字面量**污染（详见上一段）；② 想用
  `node -e` 加正则 `replace(/mod tests \{\n/, …)` 把一段测试挪进 `mod tests`，
  **匹配到了错误位置、把 `upstream/mod.rs` 整个改坏**（幸好它是 tracked 的，
  `git checkout` 恢复后用 Edit 重做）。**改大文件一律用精确字符串匹配的编辑工具，
  别用正则 replace** —— 它没有唯一性检查，出错时是静默的。
  <br>当前基线 **910 passed / 0 failed / 5 ignored**（连跑 2 次），累计 **36 条注入**全部有结论；
  clippy 干净、`npm run gates` 全绿、零棘轮抬高（`sse.rs` 两处都是一行换一行）。
  <br>**收尾把两项漏账补上（同日）**：
  <br>① **前端也做了同等的模式扫查**（64 个源文件，剥注释）：`as any` / `as unknown as`
  **0 处**、`dangerouslySetInnerHTML`/`innerHTML=`/`eval` **0 处**、
  `localStorage`/`sessionStorage` **0 处**（不往浏览器存任何东西）。9 处 `[0]` 下标与 2 处
  非空断言**逐个核过守卫，全部安全** —— `CategoryPage.tsx:372` 那处一度被我判成缺陷，
  实际上函数第一行就有 `if (enabledKeys.length < 2) return []`（我漏看了）。
  <br>📌 **建议（未做）**：`tsconfig.json` 有 `strict: true` 但没开
  `noUncheckedIndexedAccess`，所以 `arr[0]` 在类型上被当成 `T` 而非 `T | undefined` ——
  全仓那 9 处守卫全靠人工。开它能让编译器保证，代价是会冒出大量新报错要逐处处理。
  <br>② **`proxy.rs` 那两处 `expect("…必然已收集")` 复核结论：不是缺陷。**
  `tool_sets` 是 `sse_dir.map(|_| …)` 出来的，`Option::map` 的语义**已经保证**
  「`sse_dir` 是 Some ⟹ `tool_sets` 是 Some」；`resp_tool_sets` 同理由
  `(downstream != key.protocol).then(…)` 派生，与用它的分支条件是同一个判断。
  Rust 只是不知道两个 `Option` 的关联，所以今天**不会** panic。
  <br>风险在「日后有人改掉派生方式」。用类型绑成 `Option<(dir, sets)>` 能让编译器保证，
  但那是重构、`proxy.rs` 余量为 0、且不该紧挨着一次未做的真机验证做（同 docs/15 那四项
  「刻意未做」的理由）。改用零成本的机械判据钉住**来源**：
  `the_two_sse_invariants_must_stay_derived_not_assumed`，改掉派生方式就变红（已注入验证）。
  <br>当前基线 **911 passed / 0 failed / 5 ignored**（连跑 2 次），累计 **37 条注入**全部有结论。
  <br>**`noUncheckedIndexedAccess` 实测过了，结论是「刻意不开」（2026-08-28）**：
  临时打开跑 `tsc`，**45 处报错 / 13 个文件**（测试 15 + `mockData` 10 + 生产代码 20）。
  逐处核过 —— **没有一个是真 bug**，全部是「运行时有守卫或数学保证、类型系统看不出」：
  模运算下标（`FALLBACK_COLORS[h % len]`、`THEME_ORDER[(i+1) % len]`，即使 `indexOf` 返 -1 也落 0）、
  正则匹配成功后的捕获组（`m[1]`）、`Record<联合类型, T>` 索引、以及前置的
  `length < 2 → return` / `filter(models.length > 0)` / `if (!trimmed) return "?"`。
  <br>不开的理由：零当前缺陷，而 45 处里大半只能加 `!` —— **那是把编译器保证换回人工保证**，
  没有收益；测试与 `mockData` 占 25 处纯噪音。它属于 docs/15 那类「大 diff、零用户可感收益」。
  <br>**但顺带修了它指出的一处真实写法问题**：`CommandPalette.tsx` 里
  `keyResults[i].status === "fulfilled" ? keyResults[i].value : []` —— **两次索引在 TS 看来是
  两个不同的表达式**，类型收窄不生效，`.value` 在联合类型 `PromiseSettledResult` 上压根不存在
  （开了选项后报 TS2339，不是下标问题）。运行时没问题，但同一个函数里**下面那段已经是
  正确写法**（`const r = proxyResults[i]` 再判 status），两种写法并存迟早被抄错的那一种带走。
  已统一成局部变量形式。

- **流式「静默超时」已实现（2026-08-30）**：此前只有整体超时（`client::key_timeout`），
  管不住这一种形态 —— **上游先回 200、SSE 流开起来，然后中途停住不再吐字节**。连接还活着、
  没有错误，于是我们干等到总超时或对端网关掐断。用户 08-29 的日志里就是一条
  `Anthropic HTTP 524`、耗时 **259 秒**，那 259 秒里我们既没切 Key 也没告诉用户任何事。
  实现在 [`stream_idle.rs`](src-tauri/src/upstream/stream_idle.rs)（挂 `upstream` 下），
  抄的是 cc-switch 那张表里的第二档「流式静默超时 180s，数据块之间的最大间隔」。
  <br>🔴 **超时了必须注入一个 SSE `error` 事件，不能直接结束流**：直接 `return None`
  下游看到的是「流正常结束」→ `log_success` 坐实成功、清 `fail_count`、解除短路窗口，
  也就是**把一次失败静默记成成功**。注入之后两条现成路径同时接管
  （`sse_stream_errored` 认出它 → `record_live_failure`；客户端本就要处理 Anthropic 的
  `error` 事件）。这个模块因此**零新增契约** —— 它把「静默卡死」翻译成一种系统里
  已经会正确处理的失败。
  <br>⚠️ **那句文案只在同协议 Anthropic 直通时到得了用户眼前**：跨协议走 `SseTranslator`，
  而翻译器认不出这条 error、会整个丢掉，此时注入只承担「让流末判成失败」那一半
  （熔断记账仍正确），下游看到的是「流意外结束」。上游返非 SSE（少数中转站对流式回 JSON）
  或带 gzip 时同理。两种情形下响应体本来就已截断，故危害限于「提示失效」。
  **模块头写明了这条边界** —— 第一版声称「客户端会看到一句明确的报错」，那是过度承诺。
  <br>**记账口径刻意按 Key 级罚**（`record_live_failure`），而上游自己回 502/524 是**不罚**的。
  同一场网关故障谁先到就决定罚不罚 —— 取舍理由：200 之后首字节已发出、已无故障转移余地，
  熔断是此时唯一的保护。代价是链路慢的 Key 连撞三次（每次 180s）会被停 60s。
  <br>**180s 不做成可配、也不给禁用开关**：cc-switch 那张表分首字节（90s）与静默（180s）
  两档并允许填 0 禁用，我们**合并成这一个**（首字节也吃它）。理由是这个数字要么远大于
  正常间隔（没人需要调），要么调小了会误杀长思考。
  <br>3 条测试，含一条**源码级接线判据**（`both_streaming_exits_must_go_through_the_idle_guard`，
  要求 `guard_stream_idle(resp.bytes_stream())` 恰好出现 **2** 次 —— 同协议直通与跨协议翻译
  两条流式出口都得套上）。这是本仓第 9 次盯同一类接线盲区。
  <br>⚠️ 顺手修掉一条**自己写的恒真断言**：`assert!(text.contains("50") || text.contains('0'))`
  —— 50ms 超时下 `as_secs()` 为 0，而任何十进制里都可能有 0，那条 `||` 恒成立。
  已改为断言真实秒数。**「随便含个数字就算过」不是判据。**

- **扩展思考签名整流已实现（2026-08-30）**：`THINKING_SIGNATURE_INVALID` 的成因定性
  （故障转移的固有代价）一直是对的，但当时只考虑了「换不换 Key」这一个维度，给出的三条出路
  全是让**用户**动手。cc-switch 的 `thinking_rectifier.rs` 走的是第三条：**把验不过的那部分
  从请求里摘掉再试**。实现在
  [`thinking_rectify.rs`](src-tauri/src/upstream/thinking_rectify.rs)。
  <br>它还揭示了一个我们完全没覆盖的情形：**有些第三方渠道压根不接受 `signature` 字段**
  （报 `extra inputs are not permitted`）。三个场景各对应一类真实上游，判据是大小写无关的
  子串匹配（这些文案由各家上游自己拼，只有 Anthropic 给了机器码），脆但失效方向是**退回现状**。
  <br>**落点不是「同一个 Key 重打一次」**，而是就地改 `req_json` 让后续候选受益 ——
  `proxy.rs` 的候选循环把它借给每个候选（`Cow::Borrowed`，只在最后一个才 `take`），
  所以整流是**一行接线**，不用在流式与非流式两条路径里各插一段重试。
  <br>🔴 **那一行挂在候选循环的共享前段，不挂进失败分支**：失败分支有**三条**
  （流式非 2xx / 非流式非 2xx / 连接层），第一版只挂了流式那条 →
  `stream:false` 的客户端完全得不到自愈，而那是静默的。判据因此**钉住位置**而不只钉「调了」
  （`the_rectifier_must_be_wired_into_the_shared_prologue`：调用必须排在
  `if wants_stream && can_stream(key) {` 之前，且只有一处）。第 10 次同类接线盲区。
  <br>🔴 **三条「修一个 400 换来另一个 400」的坑，全都必须一起处理**（都在
  `strip_thinking_blocks` 的函数文档里，各有测试 + 注入验证）：
  ① **摘干净了就必须把顶层 `thinking` 一起关掉** —— 留着 `thinking:{type:"enabled"}` 换来
  `Expected \`thinking\` or \`redacted_thinking\`, but found \`text\``（续接工具调用的
  assistant 消息必须以思考块开头）；② **留下的思考块不许摘它的 `signature`** ——
  那个字段在思考块里是**必填**的，摘掉换来 `…signature: Field required`；
  ③ **还剩块时不许关顶层 `thinking`** ——「关着思考却带着思考块」是我们没取证的组合，
  不拿真实上游试。这三条错法的共同特征是**换来的那条错误不含 `signature` 字样**，
  既不命中本模块判据、也不命中 `annotate_known_upstream_error` → 用户拿到一句零说明的英文，
  **比整流之前更糟**（之前至少有三条出路的提示）。
  <br>⚠️ 第一版就同时踩了 ① 和 ②：②尤其讽刺 —— 模块头专门写了「删完会空的消息整条不动，
  免得把一个 400 换成另一个 400」这条边界，而**同一个函数的第二个循环无条件摘 signature、
  把这条边界自己击穿了**。原有那条测试的夹具里恰好没给 signature，所以一直是绿的。
  <br>**已知限制（两条，写在模块头免得被当 bug 重查）**：只有一个候选时不会自愈；
  「思考块独占整条 assistant 消息」这一形态仍不自愈（见 ②③）。
  <br>事件落 `failover` 而非 `config`：这是「为什么第一条 400、第二条却好了」的唯一解释，
  排障的人在「故障转移」分组里找它，落进「系统」组等于藏起来。
  <br>⚠️ `rectify_on_signature_error` 本身**曾零覆盖** —— 把落事件那 8 行整段删掉 5 条测试全绿。
  已补一条同时钉住「429 不许触发整流（否则每次上游报错都白丢一轮思考上下文）」与
  「命中时必须留痕、且 kind 是 failover」。**6 条注入全部验证变红。**

- **`allow_in_aggregate`（「允许大脑聚合使用」）已实现（2026-08-30）**：`enabled` 管
  「进不进故障转移池」，而**大脑聚合不走故障转移**（按 `keyId::model` 精确调用）。
  用户禁用一条 Key 常常**正是**因为它的模型名与主 Key 不重叠、进池会让故障转移 404 ——
  那条 Key 本身是好的、有额度的。新字段让它仍能当聚合的成员/决策者/汇总者。
  入口在 **Key 卡片**上（`KeyCard.tsx`，且只在该 Key 已禁用时渲染 —— 启用状态下多一个
  开关只会让人以为有从属关系）。`#[serde(default)]` = 老配置读进来保持原行为。
  <br>🔴 **本轮最重的一条缺陷：保存一次 Key 就把它清成 false。** `KeyEditor` 的
  `buildDraftKey()` 里没有这个字段，而保存走整份 upsert；更糟的是点一下「测试查询」
  （`:423`）也 upsert。用户勾好 → 聚合正常 → 之后为**任何**别的原因打开编辑器点保存，
  开关自己消失，下一轮聚合那位成员又被静默跳过，而他确信自己开过。方向上永不自愈。
  <br>已补 [tests/providerKeyDraftParity.test.ts](tests/providerKeyDraftParity.test.ts)
  做**双向**判据：从 `model.rs` 抽 `ProviderKey` 字段名，每个字段要么在 `buildDraftKey` 里、
  要么在 `upsert_key` 的「运行态沿用库里现值」清单里（那份清单也从 Rust 源码抽）。
  于是 Rust 加字段忘了同步前端 → 红；后端把某字段改成运行态沿用却没同步 → 红。
  另一条查「草稿里有 Rust 不认识的键」—— serde 默认忽略未知字段，**拼错一个键名不报错**，
  那个设置永远存不进去。⚠️ 判据第一版只认 `key: value` 形态，把 `vendor`/`protocol`/
  `models`/`icon` 四个 **ES 简写**字段误报成缺失。
  <br>🔴 **不要**改成在 `upsert_key` 的运行态清单里保它 —— 那个 checkbox 本身就是走
  `upsertKey` 写的，那样会把开关变成一个永远写不进去的 no-op，比现在更糟。
  <br>**四处「指错方向的提示」一起改掉**：`call_ref` 的报错、`gather_members` 的跳过事件、
  `mcp.rs` 聚合结果里那句「N 个成员因所属 Key 已停用而跳过」原先都只说「重新启用」——
  而重新启用会让这条 Key 回到故障转移池、把当初禁用它的那个 404 带回主链路，
  **等于把用户指去做一个已知有害的操作**，而他手里有一个代价为零的选项。
  第四处是 BrainPage 的「一键快速配置」：口径只认 `enabled`，于是「全部 Key 已禁用 +
  已勾选允许聚合」（这个开关设想的典型用法）下它报「没有可用的 Key、请先启用」，
  而手动逐条添加完全可用。
  <br>⚠️ **我第一版把出路写成「在 Key 编辑器里打开」—— 那里根本没有这个开关。**
  已补 [tests/aggregateDisabledKeyHint.test.ts](tests/aggregateDisabledKeyHint.test.ts)：
  **双向**判据（开关搬到哪个界面，文案就必须说哪个界面），并覆盖上面三条 Rust 文案。
  <br>🔴 **那条判据自己踩了「判对了维度」这一条**：第一版按「文件里提到 `allowInAggregate`」
  判宿主，而修 KeyEditor 那个缺陷**必须**在它里面加一行纯透传 → 判据会翻脸要求文案写
  「编辑器」，**催着人把用户送进空房间**。已改为要求同一行上既有字段、又有控件绑定形态。
  <br>余额自动查询的口径也改成 `enabled || allowInAggregate`：那样的 Key 正在被聚合真实调用、
  正在烧额度，而余额显示存在的全部意义就是别让用户看着一个过期数字。
  **健康探测仍只对 `enabled` 发**（后端口径，本轮不动）→ 这类 Key 健康状态停在「未知」，
  已知的不对称，写在代码注释里。诊断报告的 Key 摘要行加了「允许聚合=」那一位 ——
  否则「一条显示已禁用的 Key 为什么在消耗额度」这个问题，摘要行给的答案是「它是禁用的」。
  <br>**测试**：整个新语义原先**零覆盖**（把判据改回裸 `!key.enabled` 全套 926 条全绿）。
  已补一条行为用例（`call_ref` 两侧 + 报错文案）与一条源码级判据
  （两道门必须同口径、且只有两处）。4 条注入全部变红。

- **MCP stdio 这一跳补上可观测性（2026-08-30）**：桌面端与 Codex 的 MCP 走 stdio 子进程
  （`--mcp-stdio` + `--mcp-category=<分类>`），而这一跳此前**零可观测性** ——
  主应用记「我返回了结果」，Codex 记「我收到空」，中间这个进程什么都不说。
  08-29 那次排查（三次调 `synaroute_ai` 都返回空）就卡在这里：超时、MCP 注册、
  `content` 为空三种假设逐一排除后，剩下的候选全在本进程内，而现有日志一条都覆盖不到。
  <br>🔴 **`flush` 的错误此前是 `let _ =` 吞掉的**。`write_all` 只保证进了缓冲区，
  **真正让对方看见的是 flush** —— 吞掉它的失效形态正是「我们以为发出去了、对方什么也没收到」，
  也就是那次症状的候选根因之一。现在失败即留痕并退出循环（管道已不可用，继续读只会攒出
  更多无人收的响应）。`write_all` 失败也单独留痕：不然两者在日志里无从分辨。
  <br>诊断日志走 **exe 同级 `logs/mcp-stdio.log`**（不经 `Store` —— 子进程被 MSIX 客户端
  拉起会继承包身份，读 `%APPDATA%` 被虚拟化）。三条刻意行为：单独一个文件（`*.jsonl`
  由主应用写线程独占并维护滚动分片的体积记账，第二个追加者会把那份记账搞乱）、
  超过 1 MB 整份重写（这个文件名 `cleanup_old_logs_in` 解析不出日期 → **永不被保留期清理**，
  不自己设上限就是无界增长）、**绝不记 prompt / 响应正文**（正文已在主应用 trace 里，
  那里有脱敏与体积上限；这个文件落在 exe 同级、不受保留期管、用户会直接贴出来）。
  <br>🔴 **macOS 必须另走一支**，理由与 `mcp_port_file_path` 一字不差：`current_exe()` 在
  `SynaRoute.app/Contents/MacOS/` 下，写进去会被 updater 的整包替换清掉、让 codesign 的
  sealed resources 校验失败、在只读卷上直接写失败。第一版照搬了 exe 同级 ——
  **两个方向都静默**（mac 上进包内或写不出；Windows 装在 Program Files 且同级不可写时
  一行都不写），而排障者按文档以为「这一跳已经有日志了」，去找一个永远不存在的文件。
  <br>**记了什么**：`start caller=…`（进程启动就一行 —— 没有它，「文件是空的」在
  「子进程没被拉起 / 客户端一个字节都没发 / 目录不可写 / 握手那行 JSON 没解析成」
  四种成因间完全无法区分，而那正是 08-29 卡住的形态）、`recv method=…` 与 `sent bytes=…`
  配成一对（两者时间差把「主循环严格串行、一次 tools/call 期间不读 stdin」暴露出来）、
  `forward ok/err` 带耗时、非法 JSON 那条 continue（**只记长度不记内容**）。
  <br>**刻意去掉了 `result_bytes`**：它为记一个长度把整个结果再序列化一遍，而序列化失败会
  显示成 `0` —— 与「返回空」这个正在查的症状撞车。长度统一由 `sent` 那行给（真正写出去的字节数）。
  <br>🔴 **可观测性到不了排障者手上就等于没有**：这个文件此前全仓只出现在写入点一处 ——
  诊断报告不打、docs 不提、UI 不提，而报告里那行「日志目录」在用户改过 `logDir` 时指向别处。
  已在 `diagnostics.rs` 打出完整路径 + 字节数（只在文件存在时打），并有源码级判据钉住这条接线。
  <br>⚠️ **「诊断行绝不带正文」那条判据第一版按行扫，而文件里唯一的多行 `diag` 调用整个逃出了
  扫描范围**（注入实测仍绿）。已改为按「从 `diag(` 扫到分号」取整段，并加一条正向断言
  「必须扫到 ≥6 处调用」——否则调用形态一变，判据会静默退化成什么都没查。
  <br>同一条判据里加了 `!println!` / `!print!`：**stdout 是 JSON-RPC 协议信道**，
  往里写一个字节就会让 MCP「握不上手 / 工具是空壳」，而这类症状最难归因到一句调试打印上；
  而且本模式下 tracing **没有 subscriber**（stdio 早退排在 tracing init 之前），
  `warn!` 是空操作 —— 想在这里排障只能用 `diag`。3 条注入全部变红。
  <br>📌 **主循环严格串行这条已部分闭环**（详见本文档「那四项」②）：一次 `tools/call` 期间
  （最长 600s）完全不读 stdin，于是客户端 keepalive 的 ping 得不到回应 → 认为 MCP server
  已死 → 断开或杀子进程。现在 **`ping` 会被即时回答**（读隔离进独立任务，只在 cancel-safe 的
  `recv` 上 select；`read_line` 不是 cancel-safe，直接 select 它会丢半行、把 JSON-RPC 流撕开）。
  **仍未做的是真正的并发 `tools/call`**：要给 stdout 加一把 mutex，且得先取证客户端容不容得下
  乱序响应。新加的 `recv`/`sent` 时间差正是为了先把它定性。

- **本轮收尾审查（2026-08-30）另修出 8 条，逐条记判据**：
  - 🔴 **流式请求在流内失败后，日志页那一行仍然显示「成功」**（本轮唯一一条「排障时看到的是
    假现场」，而且它比静默超时更广 —— 任何流内 error 都这样，Anthropic 过载中途发 error
    是最常见形态）。流式的日志行在**拿到 200 响应头那一刻**就写下（`log_success`，kind
    `route` → 日志页绿色「路由」组），延迟记的是到响应头的耗时；流末虽然按
    `sse_stream_errored` 把**健康记账**纠正成失败，那一行**没人回头改**
    （`backfill_usage_for_collapsed_event` 只补 token 用量）。于是用户在客户端看到报错、
    回到日志页看到「成功 · 200 · 1.2s」，而它真实卡了 180 秒 —— 排障者据此判定
    「代理这边没问题」。前两次失败连系统通知都没有（要攒到熔断阈值才弹）。
    <br>**刻意不去改那一行**：`*.jsonl` 已经把「成功」那行写出去了，改内存副本会造出
    「界面说失败、文件说成功」两个平行事实（MSIX 那次惨案上吃够了平行宇宙）。
    那一行本身**不是假话**（上游确实回了 200 并开了流），缺的是「后来怎么了」。
    故**追加**一条 `error` 级可折叠事件（→ 红色「错误」组），两行合起来才是完整时间线。
    <br>收在 `health::record_stream_end`：两条流末路径（同协议直通 / 跨协议翻译）原先各写
    一遍「二选一」，改成都调它 —— 各写一遍必然漏掉一条（第 11 次同类接线盲区）。
    判据钉住 `record_stream_end(` 恰好 **2** 处，且生产段里 `record_live_success(` 只剩 **1** 处
    （非流式成功分支）—— 后者同时把那条历史缺陷「拿到 2xx 就同步记成功→该 Key 永不熔断」
    变成机械的。**`proxy.rs` 反而省出 4 行**（5 行二选一收成 1 行调用），棘轮零抬高。
    <br>📌 **副产品**：原先列为「刻意未做」的「静默超时时在应用侧再落一条事件」**已由此顺带闭环** ——
    静默超时注入的就是一条 SSE `error` 事件，`sse_stream_errored` 认得它（有测试钉住），
    于是它自然走到这条新事件上，不需要给 `stream_idle::guard` 传 `&Arc<Store>`。
    ⚠️ 已知边界：事件文案**不区分**「上游自己发的 error」与「我们判定的静默卡死」
    （两者处置不同：等/换 Key vs 查网络与中转网关）。要分辨看客户端收到的那句错误文案，
    或 `logs/*.jsonl` 里的原始尾窗。
  - 🔴 **`[1025, 2047]` 这段 max_tokens 静默不开思考**：`effort_to_thinking_budget` 的门槛
    `max_tokens < 2048` 不是官方约束，而是从「`cap = max_tokens/2` 之后不低于 1024」
    反推出来的**实现细节**。Anthropic 的硬约束只有 `budget_tokens >= 1024` 且 `< max_tokens`。
    旧门槛的失效形态最忌讳：用户选了 `high`，请求正常返回、行为毫无变化，日志里零线索。
    改后最坏是回答被截断，而截断**可见**（`was_truncated`）—— 同「最大单次输出」那条反转：
    **静默错比响亮错更糟**。⚠️ 已知边界：流式直通下 `was_truncated` 恒为 None，
    那种情形只能靠客户端看 `stop_reason`。
  - 🔴 **`.gitattributes` 没管 `.md`，「20903 字节逐字节副本」在下一次 clone 就不成立**：
    本机 `core.autocrlf=true`，而 `codex_base_instructions.md`（内嵌的官方 `prompt.md`，
    Apache-2.0）没有任何 attribute → 提交后新克隆得到 **CRLF = 21178 字节**，
    `THIRD_PARTY_NOTICES.md` 里那个可核对的字节数当场变成假话，`include_str!` 嵌进二进制的
    也不再是原文。已加 `-text`（不是 `text eol=lf`：这份文件的价值全在「一个字节都不改」），
    并把断言从 `tpl.len() > 1000` 换成 `assert_eq!(…, 20903)`。同 `.gitattributes` 里
    那句注释记的 SSE 夹具事故是**同一个坑**。
  - 🔴 **「还原时删掉我们凭空造的目录文件」这半没有任何判据**：注入（`restore_side_files`
    里不再调 `restore_one`）后 **922 条全绿、clippy 也干净**；另一种更粗的注入（`tools.rs`
    那行退回旧写法）只被 `dead_code` 挡住，而那道门对「函数还在被调、里面那半被改坏」完全无效。
    残留是**惰性**的：指针随 `config.toml` 的 `.bak` 一起消失，文件不报错、永不自愈 ——
    用户 `.codex` 目录里从此多一份约 190 KB 的死文件。已加源码级判据（两侧各一条）。
  - 🔴 **两条写着 🔴 的字段纪律没有机械判据**：`include_apps_usage_instructions`
    在 `ModelInfo` 里是 `#[serde(default = "default_true")]`（**省略即变成 true**，
    悄悄打开一个 fallback 下关着的开关），`truncation_policy` 的 mode 必须是 **bytes** 不是
    tokens（抄错这一个键会改变工具输出的截断行为）。注入实测：同时删一行 + 改一个值，
    **922 条全绿** —— `REQUIRED_KEYS` 不含前者，而它含的后者只按**存在性**检查、不看值。
    正是「判据存在 ≠ 判对了维度」。已把前者列进 `REQUIRED_KEYS`（`validate` 也随之拦住），
    并对两者各加一条**值**断言。
  - 🔴 **Key 级熔断在应用里完全不可见**：`record_live_failure` 那句注释写着
    「发系统通知 **+ 记一条告警事件**」，而代码只做了前半。系统通知可能被免打扰/焦点助手静音，
    此时这次熔断**一点痕迹都没有**；而模型级锁定那一层一直有事件 —— 两层可见性不对称。
    已补一条 `warning` 级可折叠事件（折叠键按 Key，反复进出熔断只占一行带 ×N）。
    ⚠️ 注入时踩了个坑：第一版脚本按「文件里第一个 `append_event_collapsible`」下刀，
    删的是**模型锁**那条，红的是别的用例 —— 换成按 `"warning"` 定位才验到。
  - **用户可见文案与代码分叉两处**：① `annotate_known_upstream_error` 返回给客户端的说明
    仍写着「SynaRoute 不改写思考块 —— 转发时它是原样透传的」，而本轮起我们就是在改写它 ——
    同一次故障里日志页说「已自动摘除思考块」、错误说明说「我们不改写」，用户据此排除了
    「代理动过请求」这个方向，而那恰好是真相。顺带把判据加了一支
    `redacted_thinking`（整流没覆盖的后继形态不含 `signature` 字样，漏了就零说明）。
    ② 「工具配置预览」对用户说「Codex：**只写** ~/.codex/config.toml……**不写 auth.json**」，
    而本轮新增了一个凭空创建的文件、`files` 列表也不含它；而且「不写 auth.json」本来就与
    `apply_auth_at` 矛盾（无凭据/OAuth 过期两种形态下会整份覆盖并备份）。
    预览面板是用户核对「SynaRoute 动了我哪些文件」的**唯一**界面。已如实改写并补上那一条
    `files` 条目 —— **刻意不给正文**（目录约 190 KB = 9 条 × 20 KB 提示词，
    塞进每次预览的 IPC 载荷毫无价值），只给条目数与字节数。
  - **三处过时注释按事实收窄**：`can_build` 把「多 Key 交集为空」列为空列表成因，
    而 `discoverable_models` 在交集为空时**刻意回退主 Key 的超集**、永不返回空
    （代价随之而来且是已知的：目录里可能有备用 Key 服务不了的条目）；
    `tools::apply` / `service::apply_tool_config` 仍写「`keys` 仅桌面端用到」
    「Codex 取首个写顶层 `model`」，而本轮起 `keys` 决定 Codex 的
    `supported_reasoning_levels`（**传空切片 = 档位选择器悄悄消失**）、`models` 整份进目录、
    顶层 `model` 只在缺失/不可服务时才写；`i18n.test.ts` 的文档头声称「两条结构性判据」，
    而第二条（JSX 不得硬编码中文）**全仓没有任何实现** —— 读到「有判据」的人就不会再自己查。
  - **三处「Codex 现在会自己发」带来的界面撒谎**：本轮起 Codex 会自己下发模型名与思考档位，
    于是状态条切模型的 toast 仍写「即时生效」是错的（托盘那半已改成「新会话生效」、
    状态条这半漏了 —— `get_default_model` 在**每次 session 初始化**时读 `config.toml`）；
    推理强度那个控件在 Anthropic/Responses Key 池上退成**兜底**，而 hint 还告诉用户
    「Codex 那边没有效果，在此配才生效」—— 方向完全相反的指路。已把 zh/en 共 12 处文案
    与 `inject_default_effort` 的注释一起改准。
  <br>📌 **那四项「刻意未做」已全部做完（2026-08-30 同日）**，逐条记结论 ——
  其中②的**范围被刻意收窄**，别当成没做完：
  - **① 跨协议流内 error 翻译**（[`sse_error.rs`](src-tauri/src/upstream/sse_error.rs)，
    `#[path]` 挂 `sse.rs` 下，理由同 `lan_guard`：`sse.rs` 冻结在 1247、余量为 0）。
    洞比原描述大：`SseTranslator` 的六个方向函数**没有一个读 `error`**，于是跨协议流式下
    上游 200 之后在流内报错 → 错误被丢掉 → `finish()` 照常冲刷 `response.completed` /
    `message_stop` → **下游拿到一条「成功完成的空回答」**。健康记账那一半早修过了
    （`record_health` 用原始尾窗判错），**给下游的呈现**一直没修。
    <br>🔴 **落在 `process_line` 而不是流末**：终止性错误一到就该转给下游，客户端才能立刻停；
    而流末那条路（`finish()`）**只在正常终止时走到**，连接被对端掐断时压根不执行。
    <br>🔴 **发了错误必须抑制随后的收尾事件**，否则下游先收到「失败」再收到
    `response.completed`，两条自相矛盾而客户端多半以后者为准 —— 又变回「成功的空回答」。
    **不新增字段**：`started` 已经是那个闩（两个收尾函数开头都是 `if !self.started { return 空 }`）。
    Chat 下游刻意例外 —— OpenAI 的约定是 `data: {"error":…}` 之后仍发 `[DONE]`。
    <br>顺带闭环了 `stream_idle` 的已知限制：它注入的就是一条 Anthropic 形态的 error 事件，
    走的正是这条新路径。**那个模块头里「跨协议下这句文案到不了用户眼前」已按事实改写** ——
    留着就是本仓最在意的过时注释。仍到不了的只剩「上游返的压根不是 SSE」（gzip / 少数中转站
    对流式请求回 JSON），而那时响应体本来就已截断。6 条测试，含源码级
    `process_line_must_check_for_errors_before_dispatching`。
  - **② stdio 转发期间不再无视 stdin**（`mcp/stdio.rs`）。原计划写的是「主循环改成
    per-request spawn」，实际**刻意只做了 ping**：
    <br>🔴 **`read_line` 不是 cancel-safe** —— 在主循环里直接 `select!` 一个 `read_line`，
    转发先完成时那个 future 被 drop，**已经读进缓冲的半行就丢了**，而丢半行等于把 JSON-RPC
    流撕开（此后每一行都解析失败，表现是「工具突然全坏」）。故把「读」隔离进独立任务，
    只在 cancel-safe 的 `mpsc::Receiver::recv` 上 select，容量 64 兼作背压。
    <br>🔴 **只放 `ping` 抢先回答，其余一律排队**：让两个 `tools/call` 并发是另一件事
    （两轮聚合同时烧额度、响应还会乱序而客户端未必容得下），而 `ping` 是纯本地静态回答、
    零副作用。原先主循环在这里一动不动最长 **600s**，客户端 keepalive 得不到回应 →
    认为 MCP server 已死 → 断开或杀子进程，用户看到的正是「工具不可用 / 返回空」。
    <br>**原计划挂的那个前提（「先验证客户端在 tool 调用进行中是否真的发 ping 且带超时」）
    是被取舍掉的、不是忘了**：客户端不发 ping 时即时回 ping 的成本为零，而不回的代价是 600s
    静默 —— 代价不对称，那就直接做安全的那一半。
    <br>两个坑写在代码里：`pending` 队列**先消化再取新的**（否则转发期间攒下的请求被后到的插队）；
    stdin 关了必须置 `stdin_open = false`，否则 `recv` 立刻再返 None → **忙等**。
  - **③ 状态条并列回显 Codex 自己的 `model`**（`codex_catalog::current_config_model`
    + `get_codex_config_model`，命令放在 `codex_catalog.rs` 而不是 `lib.rs` —— 余量为 0）。
    **不是把下拉改成显示真实值，而是把两个值并列摆出来**（且**只在两者不同时**才多出那一格
    —— 相同时它是纯噪音，不同时它才是「谁在生效」的答案）：下拉是**兜底**（`active_models`），
    而 Codex 会把用户在 `/model` 里的选择写回 `config.toml`、`model_choice::pick` 对 Codex
    又**优先尊重客户端发来的可服务名字**。只显示一个，在「这里选 b、Codex 里改成 a」
    这个完全正常的序列之后必然说谎。函数**只读**；`None` 的四种情形（取不到路径 / 读不出 /
    不是我们接入的 config / 没有 `model` 键）都不报错 —— 最常见的一种是「用户根本没接入 Codex」。
  - **④ 「允许大脑聚合使用」改走专用 IPC**（[`key_flags.rs`](src-tauri/src/key_flags.rs)，
    `#[path]` 挂 **`store`** 下 —— `mutate_and_persist` 是 `crate::store` 私有的、只有子模块
    调得到；用它而不是裸 `self.persist()`（后者被棘轮计数，且让内存领先磁盘）。
    <br>竞态本身**可自愈**（下次查余额重跑一遍探测链），真正的理由是**方向**：一位开关没道理
    携带 20+ 个陈旧字段，而下一个后端自管字段未必自愈。`upsert_key` 是整份替换、只沿用库里的
    `health` 与 `cached_balance`，于是点一下就把后端刚探测到的 `balance_query.url` 顶回旧值
    —— 那个字段正是「su2api 全查回 10000 USD」的修复所在。
    <br>🔴 **别改成把它加进 `upsert_key` 的运行态沿用清单**（上文那条 🔴）：checkbox 本身就是走
    `upsertKey` 写的，那样会让开关变成永远写不进去的 no-op。
    <br>Key 不存在返 `NotFound` 而**不是**静默成功：卡片可能握着一份已被删掉的 Key 的快照，
    静默成功的表现是「勾上了、刷新后自己弹回去」而用户拿不到任何线索。
    <br>Rust 侧**刻意保留了「对照的另一半」**：同一个用例里再用旧快照 `upsert_key` 一次、
    断言 url 被顶回 `""`。那是这个模块为什么存在的**可执行**证据，失败消息写明
    「这条变红意味着 `upsert_key` 现在保护了该字段，本模块的存在要重新讨论」。
    <br>`the_command_must_be_registered_in_the_handler_list` 用 `include_str!("lib.rs")` 钉住注册：
    策略门 `invoke-command-must-exist` 只查**正向**（前端调的名字在 Rust 有 `#[tauri::command]`），
    反向（写了命令却没进 `generate_handler!`）**只在用户点到那个 checkbox 时炸**。
    <br>跨语言那条 [tests/allowInAggregateWrite.test.ts](tests/allowInAggregateWrite.test.ts)
    是第 **12** 次盯同一类接线盲区：`key_flags.rs` 的两条行为用例**直调函数**，把 KeyCard 那行
    改回 `api.upsertKey({ ...k, allowInAggregate })` 它们照样全绿 —— 而那正是缺陷本体。
    判据先断言 checkbox 锚点还在（否则「没有 upsertKey」是空洞的绿），**边界也写明**：
    `KeyEditor` 保存整个 Key 照旧走 `upsertKey`（那本来就是整份替换的正当场合）。
    <br>**零棘轮抬高**：`store.rs`（2938）与 `lib.rs`（2074）余量都是 0，靠把既有两处
    `#[path]` 挂载的注释压成单行腾出，两个文件**净零**。5 条注入全部变红。
  <br>⚠️ **本轮两条新教训，都是「我自己的验证工具骗了我」**：
  <br>① **注入脚本自己踩了 CLAUDE.md 里记过的 `cp -p` 坑**：恢复时先
  `const now = new Date()` 再 `execSync`，然后 `utimesSync(file, now, now)` ——
  注入版的编译产物生成在那个时间戳**之后**，于是 cargo 判定源码未变、沿用了**注入版**的二进制。
  症状极具迷惑性：`a_vanished_key_is_reported_not_silently_accepted` 单独跑也红
  （`called Result::unwrap_err() on an Ok value`），而 grep 源码明明还是
  `None => Err(AppError::NotFound(...))`。`touch` 两个文件后 941 全绿。
  **为避开那个坑而写的脚本，把那个坑又引了回来** —— 时间戳只能往后拨，别往回对齐。
  <br>② **`| tail -1` 会把你正要拿来当证据的输出吃掉**：一次全量跑红了，而
  `for i in 1 2; do cargo test --lib 2>&1 | tail -1; done` 只打出
  `error: test failed, to rerun pass --lib` —— **用例名不可恢复**。此后 9 次全量全绿，
  且原样重跑那条命令**一个字都不打**、退出码 0。**你依赖其输出当证据的那次运行，不要接 `tail`。**
  <br>📌 **仍然刻意未做**（别当遗漏）：stdio 主循环真正的并发 `tools/call`
  —— 要给 stdout 加一把 mutex，且**得先取证客户端容不容得下乱序响应**；
  上面②新加的 `recv`/`sent` 时间差正是为定性它准备的。
  <br>当前基线：`cargo test --lib` **941 passed / 0 failed / 6 ignored**、
  `npm test` **143 passed / 20 文件**、clippy 干净、`tsc --noEmit` 干净、
  `npm run gates` 全绿、**零棘轮抬高**。


- **余额参与路由已实现（2026-08-30，docs/14 §21.1 B4 —— B 层至此清空）**：实现在
  [`balance_gate.rs`](src-tauri/src/balance_gate.rs)，`#[path]` 挂 **`health`** 下
  （`store.rs`/`lib.rs`/`proxy.rs` 三个"自然家"余量都是 0，而它与另两层弹性确实同族）。
  <br>它的作用**不是防止失败**（失败早已被故障转移兜住），而是**别等失败才知道**：
  欠费 Key 上游回 **429** 时 `TRANSIENT_4XX` 刻意不计熔断（那条规则是对的），
  代价是它永远留在候选池首位、**每个请求白耗一次往返**；回 402/403 则是连撞三次熔断 60s、
  放回来再撞三次 —— **周期性反复白打**。用熔断表达一个永不自愈的故障本身就是错的抽象。
  <br>🔴 **三条判据边界，每条对应一种误伤**（模块头有全文，各有测试 + 注入）：
  ① **只认「确定耗尽」，不设阈值** —— 跨厂商比绝对数字不可靠（Kimi 的 `available_balance`
  与 Novita 那个同名 camelCase 字段量纲都不同），百分比要 `total` 而上游给了才有；
  `remaining <= 0` 是唯一跨厂商成立的判据。② **查不到 ≠ 为零** —— `!ok`/`None`/`transient`
  一律 Unknown、不降级，与 `KeyCard` 那句「若写 `?? 0` 就会把『取不到』渲染成『余额 0』」
  同口径。③ **降级不剔除** —— 排序键 `(is_exhausted, priority)`，兜底那条路继承同一顺序；
  硬剔除会在余额数据本身错时把好 Key **完全屏蔽**（静默误伤），全判耗尽时更会让用户
  「一条都用不了」。
  <br>🔴 **数据源必须先搬后端，那是一半工作量**：余额此前**前端驱动**（唯一入口是 IPC，
  调用方只有 `KeyCard` 的 effect/轮询，而 `usePolling` 窗口不可见就停表），加上
  `cached_balance` 带 `#[serde(skip)]`（纯内存、重启即清空）—— 托盘常驻用户在请求到达时的
  余额认知**通常是空的**。现在 `check_all_categories` 每轮顺带调 `refresh_due`。
  <br>⚠️ **后台刷新刻意受三重约束**（它反转了一个刻意决定，不该反转得更多）：
  `KeyCard` 写着「窗口不可见时停表：余额查询打真实上游、消耗额度，最小化到托盘后还在后台
  烧额度是用户不会预期的」。故只在①该 Key 余额查询已启用 ②用户**已明确开启自动查询**
  （`auto_interval_min > 0`；`0` 的语义就是「不自动查」，后台替他查 = 界面撒谎 + 悄悄烧额度）
  ③该分类**代理正在跑**（不跑就不路由）三条同时成立时刷，周期用用户自己设的值
  （含 1 分钟下限与 90% 余量，同前端口径）。
  <br>📌 **由此而来的已知代价，写明免得当 bug 查**：**没开自动查询的用户，本闸门只在一次
  手动查询后的 TTL 内起作用。** 另一条路（后台无条件查）要违反用户的明确设置。
  <br>🔴 **实现中抓出一条自己写的判据顺序错误**：第一版把 `!r.ok → Unknown` 排在
  `is_valid == Some(false) → Exhausted` 之前，而 `balance.rs:525` 在「上游声明账号不可用**且**
  没给余额数字」时返回的正是 `failed(why)` + `is_valid = Some(false)`（即 `ok == false`）——
  **最可靠的那个信号**被当成「我们这侧查询失败」丢掉了。`BalanceResult` 的字段文档专门把
  `error`（我们这侧的失败）与 `is_valid`（上游明说号废了）分开过，判据不能再把它们混起来。
  <br>🔴 **主 Key 徽标口径刻意不受影响**：`enabled_keys_sorted` 是「用户配置的优先级顺序」
  这一事实的来源（徽标/托盘/状态条都用它），余额耗尽是**运行态**、不该改写配置视图 ——
  与熔断的处理完全一致（`candidates_for` 剔除熔断中的 Key，`enabled_keys_sorted` 不）。
  让徽标跟着余额跳的表现是「用户什么都没改，主 Key 自己换了一条」。有反向判据盯着。
  <br>**顺带把 170 行查询实现从 `lib.rs` 搬进本模块**（`query_and_record` 是唯一实现，
  IPC 命令与后台刷新都走它）：抄一份会让「事件 kind 分流 / 探测地址回写 / 缓存写入」
  三条各带踩坑记录的步骤裂成两份，并发去重集合也会裂开（故它改成模块内进程级 static，
  不再挂 `AppState` —— 后台刷新拿不到 `AppState`）。
  <br>耗尽落一条**可折叠 warning**（不落这层对用户完全不可见：候选顺序界面上压根没呈现），
  且**只在结论刚变化时落**；诊断报告 Key 摘要行加了「余额=」那一位。
  <br>**12 条测试、10 条注入全部变红**，含 4 条源码级接线判据（第 13 次同类盲区）。
  **零棘轮抬高，`lib.rs` 反而 2076 → 1907。**
  <br>⚠️ **注入 ⑧ 第一次仍绿**：判据写成「事件条数不变」，而那条事件**可折叠** ——
  同 collapse key 再 append 会折进原来那条（`repeat` 变 2），`len()` 恒为 1。已改为断言
  `repeat`。同那条教训：**注入不变红时先怀疑判据压根没压到那个维度。**
  <br>🔴 **B4 自己造出两处「界面/注释与代码分叉」，都已修** —— 这类由自己的改动带出来的分叉
  最容易漏，因为写的时候脑子里是新行为、旁边那段旧文字读起来也还通顺：
  ① **用户可见文案** `balance.intervalHint` 写着「窗口不可见时不查」，后台刷新之后那是**假的**
  （已改成如实说明，并把「0 = 余额也不参与路由」这个隐含依赖明说出来）；
  ② `KeyCard.tsx` 那条「最小化到托盘后还在后台烧额度是用户不会预期的」（已保留原意 +
  写明后端会为运行中的分类刷、且 `0` 仍是「一个字节都不发」的总开关）。
  <br>**顺带清掉 3 条死文案**（zh/en 共 6 行，`i18n.ts` 基线 1475 → **1469**）。
  🔴 其中 `health.down`（「● 不可用」）**不只是死键、留着有害**：HealthBadge 刻意改用了橙色的
  「● 探测不可达」+ 一句「仍会被路由」，注释原文是「避免用户以为这个 Key 已彻底停用」——
  把那个旧词留在字典里等于给下一个人留了一个**看起来正好合用**的错误文案。
  找死键照旧要先穷举**动态拼 key 的前缀**（当前 7 个），否则会误报 `t(\`前缀.${变量}\`)` 那类。
  <br>🔴 **全链路审查（docs/08 第 A6 轮）又从 B4 自己身上修出三条，两条是高风险**：
  <br>① **判据顺序错**（同上文那条，审查时才补的注入）；
  <br>② 🔴 **后台刷新搭错了车**：第一版挂在 `health::check_all_categories` 里，而那趟车
  ⓐ 在 `health_check_interval_secs == 0`（用户关掉定时探测）时整轮 `continue` ——
  余额刷新**跟着一起停**，而它依赖的是**另一个**设置（`auto_interval_min`），
  两个无关设置的隐式耦合且失效静默；ⓑ 周期被探测间隔牵着走（探测 3600s + 余额 5 分钟
  = 实际一小时才刷，用户设的数字静默失效）；ⓒ 整轮套 `timeout(period)`，余额查询吃掉探测
  预算后打出的是「**健康探测**一轮未完成」——一条指错方向的告警。
  **`lib.rs` 里 `flush_usage_if_dirty` 当初正是为 ⓐⓑ 同一理由才独立起了一趟线程**
  （注释原文「用量的丢失窗口不该被探测设置牵着走」）—— 同一个理由在这里同样成立，
  而我把它当成「现成的车」搭了上去。已改为 `balance_gate::spawn_background` 独立一趟、
  固定 60s 检查节奏（真正的查询周期仍由用户的 `auto_interval_min` 决定）。
  判据同时钉住**正反两面**：`lib.rs` 必须有那一行，`health.rs` **不许**再出现 `refresh_due(`。
  <br>③ 🔴 **事件环洪水**：后台每轮每条 Key 无条件落一条事件，而 `MAX_EVENTS` 只有 **500**
  且与路由/故障转移共用。**折叠救不了这里** —— `append_event_collapsible` 只合并**紧邻**的
  上一条，多条 Key 在同一轮里彼此穿插、跨轮压根挨不上。6 条 Key × 用户设的 1 分钟
  = 360 条/小时，一个多小时把整环冲干净（缺陷分类法第 7 类）。此前受「用户得开着那个分类页」
  天然限速，**改成后台刷新才让它成为可能** —— 这是「把一个前台动作搬到后台」时要专门想的一类
  后果。已加 `Trigger::{User,Background}`：后台只在**结论变化**时落，用户主动查时照旧每次落。
  <br>**教训**：搬一个动作到后台，要问的不只是「它还对不对」，而是**「它原先被什么天然限速，
  那个限速消失后谁会被淹」**。
  <br>顺带把 `query_key_balance` 这个 IPC 命令也搬进 `balance_gate`（同 `key_flags` /
  `codex_catalog` 的既有做法：命令跟着实现走），`lib.rs` 基线因此 1907 → **1895**。
- **「还有什么没做完」这一问查出 5 处文档/注释与代码分叉（2026-08-30 收尾）**。功能面没有新缺陷
  （FR 全绿、无 TODO/FIXME、前端无恒 `disabled` 控件、`AppSettings` 23 个字段里只有
  `upstreamRetryEnabled` 无 UI 且是刻意的），但**指路信息**有 5 处已经过期 ——
  本仓把这类算缺陷，因为照着它动手的人会做无用功或查错方向：
  - 🔴 **`proxy.rs` 流末那段注释指向一个已经做完的待办**：原文写「error 事件本身已被翻译器
    丢弃……更好的做法是翻成下游协议的 error 事件，需在 sse.rs 六个方向各加一条，属独立改动，
    见 docs 待办」。`sse_error.rs` 已经把那件事做了。已改写成这道门**今天**的真实作用：
    它刻意比 `sse_error` 的判据**更宽**（`saw_upstream_error` 用原始尾窗、认任何**非 null**
    的 `error` 字段；翻译器只认对象/字符串），差集里没有它就会退回「假成功」。
  - 🔴 **`sse_error` 模块头那句「Chat 下游仍发 `[DONE]`」在生产路径上不成立**：那是翻译器
    这一层的事实（`finish()` 对 Chat 无条件返回它），而 `proxy.rs` 上面那道更宽的门先
    `return None`、压根不调 `finish()`。**两者都不改**——门的价值（堵假成功）远大于一个
    终止符；改的是那句话，并在测试里写明「本断言守的是这一层不主动抑制它，不是用户一定收到」。
    这是「单元覆盖了组件 ≠ 覆盖了调用它的那条线」的**注释版**：组件说得对，链路把它废掉了。
  - 🔴 **`proxy.rs:1640` 写着「跨协议 SSE 翻译属已知限制」** —— 跨协议翻译早就有了
    （`SseTranslator` + 本轮的 `sse_error`），那句话会让人以为整条能力不存在。
  - 🔴 **「P2-1/P2-7/P2-8 + `AppSettings` 拆分刻意不做」是一次文档回归**：docs/15 第三批
    **2026-08-06 已全部补做**（标题原文「含原先『刻意不做』的四项」），docs/14 §12.4 也记对了；
    而 08-27 整理待办清单时又从更早的来源转述回「刻意不做」，于是 CLAUDE.md、docs/14 §21.3、
    **`ratchet.json` 的 `$rule`** 三处同时说着已经做完的事还没做。代码取证：`upstream/` 是
    18 个文件的目录（`upstream.rs` 单文件已不存在）、`service.rs` 在、`model.rs:34/:97`
    有 `CategoryMeta`+`meta()`、`:1910` 有 `UserPrefs`。**教训：「刻意不做」清单每次转述都必须
    重新对代码验一遍** —— 它比普通过时注释更贵，因为它的用途就是指挥别人不要动手/去动手。
  - **「17 个冻结文件」在四处文档里都是错的**（实际 **16** —— `mockData.ts` 抽出
    `mockData.events.ts` 后降到 819 行、已掉出 `frozen`）。同类还有 `i18n.ts` 的「241×2」
    （实际中英各 **621** 条）、`bridge.ts` 的「74 处」、`lib.rs` 的「79 个命令」。
    这些数字**判据自己都会打印**：`npm run ratchet` 报冻结条数、
    `invoke-command-must-exist` 报「78 个调用 / 83 个命令」。已把文档改成指向门而不是抄数字。
    <br>⚠️ **别用 `grep -c '#\[tauri::command\]'` 数命令**：它把注释里提到这个属性的行也算进去
    （`key_flags.rs` 测试段就有一处），实测比门多 1 个。门用的是「行首必须是该属性」+ 去重成
    `Set`，那才是对的口径。
- 🔴 **v0.1.43 的 `macOS check` 红了一条，成因是测试夹具的平台假设（2026-08-30）**：
  `codex_catalog::tests::pointer_is_ours_only_matches_our_own_file_name` 无条件断言
  `D:\somewhere\else\<CATALOG_FILE>` 是「我们的指针」，而 `Path::file_name()` 按**平台**语义
  切分 —— Windows 认 `\` 也认 `/`，**Unix 只认 `/`**。于是那串在 macOS 上被当成一个完整
  文件名 → 判否 → 红（**949 passed / 1 failed**，全套里就这一条）。
  <br>**为什么只有它红**：全仓有 30 处 Windows 风格路径夹具，其余都只当**不透明字符串**用
  （JSON 值 / 显示文案 / 脱敏输入），分隔符不参与判断；只有 `pointer_is_ours` 真去调了
  `file_name()`。改法是把那条断言加 `#[cfg(windows)]` + 补一条**平台无关**的裸文件名断言。
  <br>🔴 **刻意不把生产代码改成「Unix 上也把 `\` 当分隔符」**：反斜杠在 Unix 上是**合法的
  文件名字符**，那样一个真名叫 `weird\cc-switch-model-catalog.json` 的用户文件就会被我们
  认领并覆盖 —— 方向恰好是这个判据要防的那种（cc-switch #6087 抢用户指针）。
  **平台原生语义是对的，错的是那条夹具。**
  <br>⚠️ **Windows 侧一个门都抓不到它**：`Gates` 的 windows job 与 `Release` 的四平台构建
  都不在非 Windows 上跑 `cargo test`（`tauri-action` 只构建）。**`macos-check` 是唯一的
  非 Windows 验证者** —— 它红的时候别想着「本机是绿的所以没事」。
  <br>📌 产物**不受影响**：生产代码一个字节没动，故 v0.1.43 的包与签名无需重出。
- **刻意不修的项**（别当成遗漏重复劳动）：SmartScreen 签名告警、`retrieval.rs` 的 `cwd` 白名单、
  请求日志存明文对话（默认关闭）

- **自有诊断响应头已上线（2026-08-23，借鉴 OmniRoute 的 `X-OmniRoute-*`）**：
  `X-SynaRoute-Decision`（`key=… ; model=… ; attempts=… ; latency_ms=…` 一行可粘贴）
  + `-Key` / `-Key-Id` / `-Model` / `-Attempts` / `-Latency-Ms` / `-Upstream-Status`
  / `-Request-Id` / `-Version`。实现在 [`route_meta.rs`](src-tauri/src/route_meta.rs)。
  <br>此前**一个自有响应头都没有** —— 客户端拿到回答却无从知道走了哪条 Key、切了几次。
  对本项目还有第二重价值：**这是唯一能从「用户真实进程」取证的通道**，
  不受 MSIX 包身份影响（Claude 自己启的实例活在虚拟宇宙，其表现不代表用户）。
  <br>**结构上不可能漏挂**：`handle_request` 拆成了 wrapper + `handle_request_inner`，
  头在**唯一出口**挂。inner 有 8 个出口（模型发现/单模型检索/gateway 侧端点/读体失败/
  短路窗口/无候选/成功×2/全失败），「记得在每个出口调一次」是必然会漏的纪律，
  而漏掉的表现是静默的。有一条测试专门打非转发出口来锁住这个结构。
  <br>🔴 **头里允许携带什么有一份清单**（模块注释），且 `header_name_set_is_frozen`
  那条测试冻结了头名全集 —— 加头会变红，强制过一遍清单。**禁止**放 `base_url`
  （部分中转站把令牌放在 URL 路径里 → 等于把密钥回显给下游，`RouteMeta` 故意不留 url 字段）、
  密钥、错误消息。
- **弹性分层：新增第二层「单模型锁定」（2026-08-23，借鉴 OmniRoute 的 Model Lockout）**：
  此前只有一层（Key 级 `breaker_until`），于是**补全端点的 404**（中转站「这条 Key 的某个模型
  没开通」的常态）三次之后把整条 Key 打停 60 秒，连它本来能服务的模型一起误伤。
  <br>层间归属判据收在 `proxy::failure_scope`（**单一事实来源**，别在 health.rs 复制一份）：
  补全端点 404 → 模型级；401/其余硬 4xx → Key 级；400/422/408/409/429/5xx → 谁都不罚。
  <br>**403 刻意仍归 Key 级**：它有歧义（可能是「套餐不含该模型」也可能是「Key 被封」），
  而没有本项目自己的取证支撑把它划到模型级 —— 要改先拿到用户机器上中转站的实际响应体，别按推测改。
  <br>锁的键是**上游真实模型名**，不是对外名（同一条 Key 上多个对外名常映射到同一真实模型，
  锁对外名换个别名就绕过去了，而且失效是静默的）。退避 120s 起、×2、夹到 30min；
  一次成功 `fail_count` 减半、到 0 删条目。
  <br>**升级阀门**：同一条 Key 上同时锁着 3 个不同模型 → 武装 Key 级熔断。这是分层的配套代价 ——
  模型级刻意不罚 Key，那么一条「什么都 404」的 Key 会永远赖在候选池首位。
  阀门只数**仍生效**的锁（不数过期残留），否则历史累计锁过 3 个就会被永久反复熔断。
  <br>**模型被锁会落一条可折叠事件** —— 不落的话这一层对用户完全不可见
  （只看到第一次故障转移，之后那条 Key 被静默跳过）。
  <br>顺带改了 `record_live_success`：`fail_count` 从**清零**改成**减半**。清零留了一个洞——
  「三次里坏两次」的 Key 永远熔断不了。`breaker_until` 仍一次成功就清空，
  故「恢复的 Key 立刻回到候选池」这条性质不变。有 `a_flapping_key_eventually_trips_the_breaker`
  一条测试专盯这个洞。
  <br>**注入验证**：上述两项共做了 22 条故障注入，全部确认「去掉修复后对应测试变红」。
  其中 3 条第一次注入时「仍绿」，两条是我的 sed 写了多行模式**根本没改到代码**（假阴性），
  一条是**真的测试盲区**：`steps.min(20)` 防的是 `1i64 << 64` 的**移位 panic**（会崩在转发热路径上），
  而用例只把计数推到 33、压根没到边界。已改成推过 64。**教训同 `db_copy_path` 那条：
  注入不变红时先怀疑用例没压到边界，别当成脚本没问题。**
- **大脑聚合 MCP：分类改由「接入时」写死，不再问模型（2026-08-24，用户报的 bug）**：
  症状是客户端调 `synaroute_ai` 时反过来问用户「当前是哪个工具分类」。根因不是提示词 ——
  **服务端真的分辨不出调用方**：桌面端与 Codex 的注册形态一字不差（都是
  `command=<exe>, args=["--mcp-stdio"]`），且都转发到同一个 `/mcp`，于是只能靠
  `category` 参数当拐杖，而**模型不可能知道自己活在哪个客户端里** —— 省略即被默认成
  `claude-cli`：用错 Key 池、额度记在别的分类头上、**Codex 的聚合日志一直落在 Claude CLI 页**。
  <br>分类在「接入」那一刻本就是已知的（那时是我们自己在写客户端配置），故写进注册本身：
  CLI 的 url 写成 `/mcp/claude-cli`；两个 stdio 端的 args 加 `--mcp-category=<分类>`，
  子进程读自己 argv 后翻成同样的路径段转发 —— HTTP 与 stdio 共用**一套**解析。
  schema 里的 `category` 已摘掉（它一在，模型就会问）。
  <br>**优先级**（`mcp::resolve_caller_category`，单一决策点）：路径段 → `arguments.category`
  （仅兼容旧配置/手写 curl）→ 旧版 stdio 哨兵段 `_stdio` 在「桌面端/Codex」里排除出唯一那个
  → `claude-cli`。后三档各落一条**每次运行每分类只一条**的可见事件（不节流就会把有用事件
  挤出 `MAX_EVENTS` 环，与已修过的「短路窗口每次重发记一条」同一个坑）。
  <br>**刻意不做**：认不出就报错。升级后应用启动 `rewrite_registered_clients` 已自动重写三端
  配置，但客户端要**重启一次**才读到；那段窗口里报错=工具直接不可用，代价不对称。
  <br>**结构上的连带改动**：`mcp.rs` 当时 889/900 顶着新文件上限，故把 stdio 层拆成
  `mcp/stdio.rs`（`mcp::stdio` 子模块）。**没有抬高任何棘轮上限**，`SettingsPage.tsx` 反而
  1907→1900。设置页那个复制框也从裸 `/mcp` 改成按分类三条（抽出 `McpAddressList.tsx`）。
  <br>**注入验证 14 条全部变红**，其中**两条**第一次注入时仍绿，都是真盲区：
  ① tools.rs 那侧只验证「给它带分类段的 url 就会写下去」，而「谁决定 url 带不带分类段」
  在 service 层，改回裸基址它照样全绿 → 补了
  `client_url_round_trips_through_caller_from_path`；
  ② **`handle_http` 里「取 path → 传给 dispatch」这一步接线**压根没人覆盖 ——
  把它硬编码成 `McpCaller::Unbound`，全套 776 条**全绿**，而那就是本缺陷本身
  （每个客户端都静默退回 claude-cli）→ 补了 `handle_http_derives_the_caller_from_the_request_path`
  （真 bind 端口 0 起 HTTP、直接挂 `handle_http`；**刻意不走 `McpManager::start`**，
  它会写 exe 同级端口文件、与那条端口文件用例抢同一个文件 → 会引入偶发红）。
  <br>**教训**：单元覆盖了「解析函数」和「分发函数」不等于覆盖了**两者之间的接线**，
  而接线漏了的表现恰恰是静默的。同 CLAUDE.md 里 `route_meta` 那条「记得在每个出口调一次
  是必然会漏的纪律」。
  <br>另加一条**跨语言**判据 `tests/mcpEndpointParity.test.ts`：前端那份地址与 Rust
  `mcp::client_url` 分叉时变红（编译器管不到这条缝，而分叉是静默的）。
- **用户报的四条已全部修完（2026-08-26）**，逐条记结论与判据：
  - **Codex 401 `Incorrect API key provided: synarout***roxy`** —— 真因**不是**旧注释写的
    「provider 表被丢掉而选中项留着」。隔离 `CODEX_HOME` 实测（codex-cli 0.148.0-alpha.9）：
    顶层 `model_provider` **键整个缺失**才会回落内置官方地址、拿 auth.json 里我们的占位符去打
    `api.openai.com`（逐字复现了用户那句报错）；而「选中项悬空」Codex **启动即硬报错
    `Model provider ... not found`、一个请求都不发**；`[model_providers.openai]` 想覆盖内置 id
    直接 `reserved built-in provider IDs` 拒启动（**这条路已证伪，别再试**）。
    <br>🔴 **决定性判据：`experimental_bearer_token` 优先于 auth.json，且 0.148 没有凭据门禁**
    （本地探针抓 `Authorization` 头，四种组合全测过）。所以那份占位符在正常接入时**从不外发**，
    纯粹是负债 —— **本版起不再写 `auth.json`**，接入只动 `config.toml`，
    并在 apply/restore 时顺手**解除**旧版留下的占位符。用户的 ChatGPT 登录态从此完全不被触碰。
    <br>2026-08-02 那条旧判据（「`requires_openai_auth=true` 必须配套 auth.json，否则停在登录页」）
    对当前版本不成立；取舍不是赌版本，是**代价不对称**：万一某版本仍要凭据，它报的是
    `no Codex credentials were found · Run codex login`（响亮、可自助、OAuth 完好），
    而写占位符的代价是把假凭据发给第三方 + 一句指错方向的报错。
    <br>🔴 **`requires_openai_auth = true` 必须写，别再顺手删**（2026-08-26 用户报障后改回）：
    本轮曾一度删掉它，理由写的是「它把 Codex 推向 auth.json 那条凭据链」—— **那个理由是错的**。
    5 组探针实测（三种写法 × 有无 auth.json）：只要 `experimental_bearer_token` 在，
    `true`/`false`/省略三者发出的 `Authorization` 头**逐字节相同**、都不读 auth.json，
    也就是写它**零代价**。而不写它有代价 —— 用户报的升级公告：新版 Codex 不再允许自定义 provider
    在 `requires_openai_auth = false` 时继承 auth.json 鉴权，报 `API_KEY_REQUIRED` / 401，
    官方解法就是改成 `true`。那条在 0.148.0-alpha.9 上**复现不出**（`false` 仍然继承成功），
    说明属于更新的版本 —— 正因复现不出才不能赌。三条旁证都指向 `true`：cc-switch 生成的生效配置、
    用户自己那份能用的 `[model_providers.custom]`、官方升级公告。
    <br>**这个字段的语义已经变过两次**（08-02 一次、08-26 一次），故它也进了 `is_intact`：
    被外部改成 `false` 或删掉即判「不完好」→ 漂移告警会报、下一次接入自动纠正回来，
    用户不必自己去 config.toml 手改那一行。有 `requires_openai_auth_is_written_as_true`
    与 `intact_requires_our_endpoint_and_our_bearer` 两条测试钉住（各做过故障注入）。
    <br>另修：`obj.len() == 1` 那个占位符判据被 `codex login --with-api-key` 写出的
    **两字段**形态击穿（失效方向是**静默放行**，三道守卫同时哑掉）；
    `is_intact` 只查 base_url **非空**（指向第三方或已死旧端口都判「完好」→ 漂移告警永不发出）；
    `with_rollback` **不含副文件**（`.bak`/`.synaroute-created`）→ 一条**数据丢失级**链路：
    回滚留下 marker → 用户重新 `codex login` → 下次 apply 跳过备份 → 还原时按 marker 删文件
    → ChatGPT 登录态永久消失且盘上无副本。漂移告警改成按形态分支（回落官方/选中他人/
    选中项悬空/我们的表指向别处/`--profile` 旁路/遗留 `profile=` 键），
    **每支说的都是那个形态下 Codex 实际的行为** —— 指错方向的告警比没有告警更糟。
    <br>代码抽到 `src-tauri/src/tools/codex.rs`（tools.rs 棘轮余量为 0）。
  - **用量统计「花费」列算不出来** —— 是两件独立的事叠在一起，**修一件用户看不到变化**：
    ① 大脑聚合的 6 处 `append_event_full` 把 `key_id` 全传 `None`，而累加器键是
    `(分类, key_id.unwrap_or_default())` → 全落进 `(分类, "")` 那个桶 → 用量页显示「（系统级）」、
    查不到 Key 就取不到代表模型与倍率 → 金额恒「—」。**key_id 在那 6 处全都在作用域里**
    （`keyId::model` 引用 / `BrainMember.key_id`），不是拿不到、是记账时扔了。已加
    `ref_key_id` + `MemberCallMeta.key_id`，并有**源码级**判据
    `aggregate_usage_is_never_recorded_without_a_key` 盯着（「记得传 key_id」是必然会漏的纪律）。
    ② 单价表停在**退役价**：`("opus", 15.0, 75.0)` 是 Opus 4/4.1 的价，现役 4.5~5 是 $5/$25
    → 全仓金额恒为真值 **3 倍**（拿用户真实 usage.json 实算过：$13772 vs $4590）；
    `claude-fable-5` 落到裸 `claude` 的 $3 → **低估 3.3 倍**；无归一 + 裸 `contains`
    让 `gpt-4.1-nano` 命中 `gpt-4` → 输入价**高估 100 倍**；整个 gpt-5/o 系压根不在表里（Unknown）；
    `PricingSource::Exact` 在现实中**不可达**（内置表只有 9 条全等匹配的退役名）→ 每行都带「≈」，
    两个视觉档位退化成一个。
    <br>重建为 `pricing/{mod,table}.rs`：**归一（`.`/`/`/`_` → `-`）+ 最长片段命中**
    （表序不再是语义的一部分），家族兜底行 = `FAMILY_FLAGSHIP` 显式指向的**现役旗舰**，
    缓存价按厂商实测比例显式给值（DeepSeek 0.033×、gpt-4o 0.5×…，写死 ÷10 对多数厂商偏 2~5 倍）。
    六条**机械不变量**各带故障注入：表序无关 / 覆盖名单不落 Unknown / 兜底=旗舰 /
    旗舰必须在役 / 片段不跨厂商误吃 / 免费行不被家族兜底开账单。
    <br>「—」的四种成因（聚合无归属 / Key 已删 / 无模型名 / 模型不在表）分开报，
    **只有第三种**才给「去设默认兜底模型」那句 —— 旧文案对另三种都是假话，会把用户送去做无效操作。
    「累计花费」那格加「＋n 行未计入」角标（此前静默丢掉无价行，标着「累计」却不是总计）。
  - **内置厂商 6 → 33、图标 32 个真 logo**：每条 `base_url` 都做过**可证伪的探测**
    （bogus key 打 chat 端点，**401/403 = 路由存在、404 = 不存在**）—— 比读文档可靠，
    好几家文档自身就不自洽。坑都记在 `vendors.rs` 模块头：Groq 是 `/openai/v1` 而 DeepInfra 是
    `/v1/openai`（恰好相反）、Fireworks 把模型名的小数点写成字母 `p`、SiliconFlow 的 `Pro/` 前缀、
    Together 与 Novita 同款权重的 id **大小写不同**、Ark 的 `model` 很多账号要填「推理接入点 ID」、
    讯飞要填 APIPassword 而不是 APIKey:APISecret 那一对。
    <br>拿不到权威来源的一律 `context_window: None`（语义是「未取证」，不是「无限制」）。
    <br>🔴 **`builtin_seed()` 只在 `vendors.is_empty()` 时注入** —— 不补迁移的话
    「加了厂商但老用户看不到」，是个典型静默失效。已补「升级时补入新增内置项、
    不覆盖用户改过的、不复活用户删掉的」，并有回归测试。
    <br>深色模式下纯黑品牌（OpenAI/Kimi/xAI/Cohere/Meta）改走浅灰前景 + 深色底片，
    不再有「看不见的图标」。
  - **官网 Safari/iOS 15.0~15.3 整站白屏**（用户点名的「苹果浏览器」）：`marked` 用了 15 处
    `Array.prototype.at`，那是 **15.4+** 才有的 API，而 Vite 的 build target 只降**语法**、
    esbuild **从不补 API**。React 18 对未捕获的渲染异常会**卸载整个 root** →
    实测比预估更糟：白屏之后**连首页也回不来**（root 已空）。修法是 polyfill + 把
    `ErrorBoundary` 放在 `<Outlet/>` 外 / Header·Footer **内**（出错也还能自己导航走）。
    <br>顺带把 build target 与 browserslist 写死（原先靠 Vite 默认值、会随版本漂）——
    browserslist 一加，autoprefixer 立刻补出 **`-webkit-backdrop-filter`**，
    此前整份 CSS 里一个都没有，也就是说 Safari < 18 的顶栏毛玻璃**从未生效**。
  - **新增两条跨语言判据**（编译器管不到、分叉是静默的）：
    `vendors::tests::every_vendor_id_appears_in_the_frontend_brand_keywords`（内置厂商
    在前端认不出 → 界面退化成首字母块）与 `tests/vendorSeedParity.test.ts`（演示数据的厂商清单
    与 Rust 分叉 → **官网截图会对外少报 27 条**，因为那些图就是从演示模式截的）。
    后者的**边界写在测试头部**：只比 id/base_url/协议，不比预设模型 ——
    一个部分为真的判据必须把边界写明。
  <br>⚠️ 零一万物的关键词**刻意不收裸 `yi`**：`resolveBrand` 是子串匹配，
  `yi` 会把 `gemini` / `claude-opus` 这类名字误判成零一万物。
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

它查五项：① `git ls-files` 不含运行数据/明文密钥；② **仓库内容里没有私钥材料或签名口令**
（按解码后的内容特征判，见上一节 `no-secrets-in-tracked-files` —— 2026-08-14 的真实泄露就是
从「按文件名判」那个缝里过去的）；③ `bundle.resources`/`externalBin` 无仓库外引用；
④ `dist/` 无数据文件、无密钥内容、无演示数据集（生产必须走 `mockData.prod.ts` 空桩）；
⑤ **当次版本**的产物二进制里搜不到私钥/真实密钥/密钥库内容。

两个已踩过的判据坑（脚本注释里也记着）：**别用 i18n 占位文案当演示数据的判据**（`厂商1（官方直连）` 是输入框灰字提示，每个包都有，会对干净的包报假警）；**别查开发机用户名**（Rust 把 `.cargo/registry` 源码路径嵌进二进制供 panic 用，每个 Rust 程序都有，不含用户数据）。签名密钥只经 `TAURI_SIGNING_PRIVATE_KEY` 环境变量传入，永不落仓库。

完整流程、验证判据、部署步骤见 [docs/04-构建部署指南.md](docs/04-构建部署指南.md)。

## ⚠️ MSIX AppData 虚拟化陷阱（本项目最大惨案，务必先读）

**Claude 桌面应用是 MSIX 打包**（包家族 `Claude_pzs8sxrjxfjjc`）。Windows 对包内进程做 AppData 虚拟化：**Claude Code 及其派生的一切子进程**（Bash/powershell/node/它启动的 SynaRoute）读写 `%APPDATA%\SynaRoute\*` 时，被透明重定向到 `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\SynaRoute\*` 的**包内私有副本**。用户双击 exe 无包身份，读写**真实文件**。→ 两个平行宇宙（曾致「用户 UI 4 条 Key vs Claude 处处验证 6 条」多日排查惨案）。

由此的铁律：
1. **诊断/修改用户真实 `%APPDATA%` 数据必须逃逸包身份**：`schtasks /Create ... /TR "F:\...\x.bat"` + `/Run`（计划任务服务启动无包身份）；bat 与产物放非 AppData 盘（F:\、E:\ 不被虚拟化）。Git Bash 下 schtasks 需 `MSYS_NO_PATHCONV=1`（用完 unset，否则破坏 `taskkill //F`）。
2. **交付验证必须让用户亲自双击启动**，用共享的 exe 同级 `logs\`（非虚拟化）里的「启动自检」行核对（该日志记录每实例实际配置路径/keys 数/用户/exe，正为此设，勿删）。Claude 自己启动的实例活在虚拟宇宙，其表现不代表用户。
3. 症状特征（复发速查）：同一 exe 用户开与 Claude 开数据不同；Claude 探针全对但用户仍旧；`%APPDATA%` 目录清单在 Claude 视角与计划任务视角不同。
4. 不要删包容器里的虚拟副本（可能留 tombstone 遮蔽真实文件）。

## 🚦 质量门（2026-08-23 新增，借鉴 OmniRoute）

`npm run gates` —— 每次改动都该跑，CI 也跑（`.github/workflows/gates.yml`）。分**两类**，
别混（这个分类本身是从 OmniRoute 抄的：policy gate 与 ratchet 的治理方式完全不同）：

| 类 | 命令 | 语义 |
|---|---|---|
| **策略门** | `npm run check:forbidden` | 判据必须为 **0**，**只能修，不能冻结** |
| **棘轮** | `npm run ratchet` | 现状不理想但可接受，只要求**不变坏** |
| **棘轮防作弊** | `npm run ratchet:verify` | 基线只准往好的方向动；抬高必须附理由 |

### 策略门现有六条

> 第六条 `data-dir-env-name-must-match` 的来由与踩坑记在本文档
> 「产物门」一节里 `SYNAROUTE_DATA_DIR` 那条下面，不在这里重复。

- `no-hardcoded-local-paths` —— 把「禁止硬编码本机路径」这条既有铁律变成机械判据。
  **只查生产段的非注释行**（`.rs` 排除尾部 `#[cfg(test)] mod tests`）。第一版裸 grep
  `C:\Users` 把 11 处**文档示例与测试夹具**全报成违规 —— 同 CLAUDE.md 里
  「别用 i18n 占位文案当演示数据判据」那个教训。
- `invoke-command-must-exist` —— 前端调的每个命令名必须在 Rust 侧有 `#[tauri::command]`。
  拼错**没有编译错误**，只在用户点到那个按钮时炸。
  ⚠️ 第一版查 `invoke("...")`，**实测命中 0 处** —— 本仓真实形态是 bridge.ts 的
  `call<T>("cmd", …)`（73 处）。故脚本现在对「解析到 0 个」**主动判失败**，
  不让一个恒绿的门悄悄空转。
- `no-secrets-in-tracked-files`（2026-08-23 新增）—— 仓库里不许有私钥材料或签名凭据口令。
  实现在 [`scripts/lib/secret-scan.mjs`](scripts/lib/secret-scan.mjs)，单独跑
  `npm run check:secrets`，`audit:release` 也调它做发版前第二道。
  <br>🔴 **它补的是一次真实泄露**：2026-08-14 的 `29b9257` 把更新签名私钥
  （`DE14C6EC68286277`）连同解密口令一起写进 `GITHUB_SECRETS_SETUP.md` 正文，
  推上**公开**仓库，08-23 才发现。那把钥恰好 08-16 被换掉，故只有 **v0.1.23**
  那批客户端嵌着它 —— 是运气，不是防线。而 v0.1.23 用户处境最坏：正版更新收不到
  （0.1.26+ 用新钥签），伪造更新收得到。
  <br>🔴 **教训是「判据存在 ≠ 判对了维度」**：当时 `audit:release` 已经在跑，它按
  **文件名**判（`\.key$|\.pem$`）；而且**即使改成内容 grep `untrusted comment: rsign…`
  也照样抓不到** —— `TAURI_SIGNING_PRIVATE_KEY` 的形态是「整份密钥文件再 base64 一层」，
  那句注释在文件里根本不以明文出现。故本判据的核心是**先解 base64 再匹配**（解两层）。
  <br>另两条判据：签名凭据标签（`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 等一小类，
  **刻意不泛化成任何 `password`** —— 本应用自己有主口令功能，泛化必然淹没在假警里）
  后面跟的非占位值；以及**拿仓库里出现过的口令去试解仓库里的密文**
  （这次就是这么破的：`secrets/synaroute.key.gpg` 的 GPG 口令写在另一个 tracked 的 `.md` 里，
  「密文可公开」的前提自我作废）。
  <br>**边界写在代码注释里，别忘**：只扫工作区，**不扫 git 历史、不扫 fork**。
  门变绿只代表「没有新增」，已公开的密钥必须当作永久失效处置。
  <br>4 条故障注入均已验证变红：关掉 base64 解码 / `B64_MIN_RUN` 60→8 /
  去掉口令的前向窗口 / 去掉「围栏外只接受不含冒号的裸值行」那道假警防线。
  ⚠️ 其中一条第一次注入时**仍绿**：原先写成「run 下限 60 + 前缀下限 200」两个常量，
  把 200 改成 8 被 60 那道先挡住了 —— 那个 200 既是死数又制造了 60~199 的盲区。
  已收成单个 `B64_MIN_RUN`。**教训同 `db_copy_path` 那条：注入不变红时先怀疑判据本身重复或没压到边界。**
  <br>📌 **换钥必读**：钥匙台账（三把钥、两次换钥、各覆盖哪些版本）在
  [`secrets/README.md`](secrets/README.md)。原先那份只记了一次换钥，**漏记的正好是泄露的那把**，
  导致排查时误判成「泄露的是现役钥」。换钥必须同步更新那张表。
  现役 `7A46ECB8087DE26F` 未泄露 —— **未泄露就不要换**，换一次就再甩掉一批收不到更新的用户。
- `version-must-be-consistent`（2026-08-27 新增）—— 版本号必须处处一致。
  <br>🔴 **本条修的是上面那句「三处版本号一致」本身就是错的**：实际有**四个**文件、
  **五处**字段带版本号，而第四个（`package-lock.json` 的 `.version` 与
  `.packages[""].version`）没人管 —— 实测它停在 **0.1.33**，其余三处已是 0.1.39，
  落后 6 个版本、零告警。危害不在构建（Tauri 不读 lock 的 version，
  `npm ci` 也只对账依赖、不看顶层版本），而在**取证**：排查「应用到底报了哪个版本」时
  仓库里同时存在两个答案，却没有东西指出哪个权威。这一轮就是这么多花了一趟。
  <br>**为什么 `tauri.conf.json` 与 `Cargo.toml` 必须一并查**：它们是**两个不同的版本来源**。
  `package_info().version`（`get_app_version` / `check_for_updates` / 诊断导出）取自
  `tauri.conf.json` 的 `version`（tauri-codegen `context.rs:273`，该字段缺失时才回落
  `CARGO_PKG_VERSION`），Windows VERSIONINFO 的 FileVersion 取自**同一字段**
  （tauri-build `lib.rs:632`）；而 `x-synaroute-version` 响应头取自 `CARGO_PKG_VERSION`
  即 `Cargo.toml`。两者一旦漂移，exe 属性页与响应头会给出两个版本且**不报错**。
  <br>**刻意不查 `Cargo.lock`**：cargo 每次构建自己会对齐它，收进来只会让
  「刚 bump 完还没构建」这个正常中间态变红，是纯摩擦。
  <br>同规则 2/3 的教训，**解析到少于 5 处就主动判失败** —— 文件改名或字段挪走会让
  「全部一致」静默退化成「一个都没查」。三条注入均验证变红：lock 退回 0.1.33 /
  `tauri.conf.json` 与 `Cargo.toml` 互不一致 / 删掉 `packages[""]` 让字段数掉到 4。
  <br>修 lock 用 `npm install --package-lock-only`，别手改。
- `updater-must-read-system-proxy`（2026-08-27 新增）—— 应用内更新必须保留读 Windows
  系统代理的能力。
  <br>🔴 **这个能力此前是「偶然」得来的，而一旦失去是静默的**：`reqwest` **无条件**调
  `hyper-util` 的 `Matcher::from_system()`（`reqwest-0.13.4/src/proxy.rs:517`，无 feature 门控），
  而真正读注册表 `ProxyEnable`/`ProxyServer` 那段被 `hyper-util` 的
  `client-proxy-system` feature 门控（`matcher.rs:245` 与 `:663`）。
  本仓从未显式要求过那个 feature —— 是 `Cargo.toml` 写了
  `hyper-util = { features = ["full"] }` 恰好把它带进来的。
  谁为瘦身收窄 `full`，**双击启动**的实例就再也读不到系统代理
  （它不继承任何 shell 的 `HTTP_PROXY`）→ 国内用户「检查更新」全部超时，
  而编译、测试、其余门**都不报错**。
  <br>判据两条：① `Cargo.toml` 的 hyper-util 含 `full` 或显式含 `client-proxy-system`；
  ② `Cargo.lock` 里 hyper-util **只有一个版本** —— feature 是按包并集的，
  出现第二个版本时第一条会「绿着但能力已失」，这是本判据唯一的盲区，故一并钉住。
  两条都做过故障注入（收窄 features / 伪造第二个版本）。
  <br>⚠️ **注入手法上又踩了同一个坑**：伪造第二个版本那次「注入后仍绿」，
  第一反应该是怀疑注入本身 —— `Cargo.lock` 是 **CRLF**，而我的注入脚本用 LF 搜索串，
  `String.replace` 静默没匹配、压根没改到文件。判据自己用的是 `\r?\n`，是对的。
  同 CLAUDE.md 里 `db_copy_path` 与 `sed 多行模式` 那两条：**注入不变红时先怀疑注入没生效**。

### 顺带修掉的一处判据漂移（2026-08-27）

`check-forbidden.mjs` 与 `check-ratchet.mjs` 各自写了一份「测试段起始行」判据，
且**已经漂移**：ratchet 那份认 `pub(crate) mod tests`（注释里记着代价：
第一版收窄导致 service.rs 从 1608 报到 2478），而 forbidden 那份是收窄版。
<br>在 forbidden 里那个盲区的失效方向是**隐蔽的**：测试段被当成生产段去扫，
于是测试夹具里的假路径会被 `no-hardcoded-local-paths` 报成违规 ——
正是该文件注释开头记的第一个判据坑。当时没报，只是因为那些夹具里恰好没有 `C:\Users`。
<br>已抽到 [`scripts/lib/rust-source.mjs`](scripts/lib/rust-source.mjs) 做单一事实来源。
**别直接 `import` 另一个门脚本**：`check-ratchet.mjs` 一被导入就跑完整棘轮并打印，
于是策略门的输出里混进棘轮的输出、检查还跑了两遍（第一版就是这样，已改为纯模块）。

### 棘轮：为什么只数「生产段」

Rust 测试是**同文件内联**的。若按整文件行数冻结，「补一条回归测试」就会被门挡住 ——
那会让质量门变成质量的敌人，而本仓的核心纪律恰恰是每个缺陷都留一条回归测试。
故 `.rs` 只数尾部 `#[cfg(test)] mod tests` 之前的行。判据有对应注入验证：
只在测试段加 50 行必须**仍绿**。

当前基线在 `config/quality/ratchet.json`：**16 个**冻结文件、新文件上限 900 行、
1 项计数（`bare_persist_calls: 3`，对上 docs/14 勘误里那条）。
🔴 **本文档此前写「17 个」，那是错的** —— `frozen` 一直是 16 条（`git show HEAD` 核对过），
而门自己每次都打印真实条数。**判据自己会报数时，别在文档里另抄一份。**

### 🔴 想抬高一个上限时的规矩

在 `ratchet.json` 里加：

```json
"raises": { "<同名 key>": { "from": 旧值, "to": 新值, "why": "为什么必须抬高（≥20 字）", "when": "YYYY-MM-DD" } }
```

否则 `ratchet:verify` 拦住。这条约束不是洁癖，是 OmniRoute 用一整节文档
（issue #8584）记下来的实测代价：**抬高**上限是十秒钟的 JSON 编辑、是解红最快的路；
**降低**要有人主动跑 `--update`。结果他们 18 个已冻结文件早就远低于上限
（最夸张一条 19 行代码背着 2523 的上限，132×）、复杂度上限从 1794 走到 2169 只降过一次、
「下个周期收紧」写了 31 次兑现 1 次。他们的结论原样抄下来：
**能抬高上限的机器人比没有机器人更糟。**

`ratchet:verify` 拦的六类动作（各有注入验证）：抬高冻结值 / 抬高新文件上限 /
抬高计数 / **删掉整条计数判据**（悄悄废规则比抬高更严重）/ 理由太短 /
理由的 `to` 与实际新值对不上。

### 产物门（只在 release 流程跑）

| 命令 | 判据 |
|---|---|
| `npm run check:embedded` | `dist/assets/` 的 chunk 名能在产物二进制里搜到（> 0）。裸 `cargo build` 该值为 0 → 装上去界面一片空白。**双向验过**：旧 exe 0/7 报红、`tauri build` 后 7/7 绿 |
| `npm run audit:release` | 既有的外泄审计（运行数据/密钥/演示数据不得进包） |
| `npm run smoke:installer` | **真装真启动**：静默装 NSIS 包 → 启动 exe → 等它自己写出「启动自检」那行 → 收尾 |

`smoke:installer` 补的是 CLAUDE.md 里挂了很久的那句「**仍未验证：NSIS 安装器能否装成**」——
也就是说交付给用户的那个包**从未被任何流程执行过**。OmniRoute 的
`check-pack-boot.mjs` 是为一模一样的事故建的（连续三个版本发出开机就崩的包，
根因是「结构检查验的是列表，不是运行时」）。

启动判据刻意用**产物自己写的那行日志**（exe 同级 `logs\`，非虚拟化），
而不是看窗口或退出码 —— 与 OmniRoute 轮询 `/api/monitoring/health` 是同一手法：
等产物自己说话。

⚠️ **本地跑 `smoke:installer` 需要 `--yes-install-on-this-machine`**：静默安装会写
`HKCU\…\Uninstall\SynaRoute` 并可能关掉你正在用的实例，于是可能顶掉你自己那份安装的
卸载入口。CI 上无此顾虑（一次性环境），故 CI 里不需要这个参数。

🟡 **`smoke:installer` 本机可跑了（2026-08-27），但需要你先退出正在跑的实例** ——
数据目录隔离已实现（下面那条原「待办」已做完），**但实测发现第二道障碍**：
产品用了 `tauri-plugin-single-instance`，已有实例在跑时冒烟实例**启动即退出**、
连自检行都不写（实测只留下一个 0 字节的 jsonl）。
不拦的话本门表现为「等 boot 超时」，而那个错误长得跟「产物坏了、起不来」一模一样，
会把人送去查安装包 —— 故脚本加了 `assertNoRunningInstance()`，在**安装之前**就判定并说清楚
（装完再发现起不来，注册表项已经写下去了）。

下面这段是隔离做出来之前的历史记录，保留因为那两条实测判据仍然有效：

🔴 **`smoke:installer` 曾只能在 CI 跑，本机一律红** —— 这是 2026-08-23 第一次真跑时
发现的：那条自检行报 `配置=%APPDATA%\SynaRoute\config.json · keys=13`，
**冒烟实例读的是用户真实配置**。而真实配置里 `proxyRunningCategories` 非空，
于是它开机会自动启动代理并顺带写工具配置（起=写、停=还原）。
那次没造成损害纯属运气：脚本一看到自检行就收尾，早于代理自启动写完
（已核对 `claude_desktop_config.json` mtime 仍是一个多月前、无 `.synaroute-created`、
config.json 13 keys 完好）。慢一点、或那条分类是 claude-cli，就会把
`~/.claude/settings.json` 改写成指向一个随后被 kill 的临时实例的端口，
而「起了没还原」这种状态**不自愈**。第二重问题：与用户自己那份实例抢同一个代理端口。

已加一条**可证伪的硬判据**：自检行的 `keys=` 必须为 0，否则整个门红（一个静默失去
隔离的门比一个红的门糟得多）。故本机跑到那一步必然红 —— 但**前两步是真过了的**，
「装得上、起得来」这个结论成立，红的只是隔离这一项。

<br>❌ **已证伪的修法，别再试**：给子进程传 `env: { APPDATA: <临时目录> }`。
`dirs::data_dir()`（`store.rs` 解析配置路径用的）在 Windows 上走
`SHGetKnownFolderPath(FOLDERID_RoamingAppData)` 这个已知文件夹 API，**不读 `APPDATA`
环境变量**，改了照样 `keys=13`。

<br>✅ **已做完（2026-08-27）**：生产侧的 `SYNAROUTE_DATA_DIR` 覆盖在
[`src-tauri/src/data_dir.rs`](src-tauri/src/data_dir.rs)（同 OmniRoute 的 `DATA_DIR`，
它的 `check-pack-boot.mjs` 注释写着 "DATA_DIR isolated"）。
挂载用 `#[path]` 挂在 `store.rs` 下 —— 因为 `store.rs` 与 `lib.rs` 棘轮余量都是 0，
而目录化是 docs/15 P2-7 刻意未做的大 diff。**零棘轮抬高**：`app_data_dir()` 自己返回
`AppResult` 并把 `SynaRoute` 子目录拼好，于是 `Store::init` 里那三行收成一行，
正好抵掉模块声明的两行。
<br>**实测验证**（不装包、直接跑产物 exe）：日志报
`配置文件不存在,使用默认空配置: "…\Temp\sr-iso-1274\config.json"`，
隔离目录里生成了 config.json，而真实 `%APPDATA%\SynaRoute\config.json`
mtime 与 keys 数（14）**均未变**。
<br>**空串必须视为未设置**：CI 里 `env: { SYNAROUTE_DATA_DIR: "" }` 是常见写法，
当成有效值会把配置写到**进程当前工作目录**的相对路径 —— 比读到真实配置更糟
（产物目录里凭空多出一份密钥库，没人会想到去那里找）。有注入验证。
<br>**变量名是跨语言契约**，已加第 6 条策略门 `data-dir-env-name-must-match`：
Rust 侧 `ENV_OVERRIDE` 与 `smoke-installer.mjs` 里 spawn 传的名字必须一致。
分叉的失效方向是**门变绿** —— 传一个产品不认的变量，产品就按老路径读真实配置。
<br>⚠️ 那条门的第一版**报了假警**：它对脚本全文 grep `env: { APPDATA:`，
命中的是脚本自己那段「❌ 已证伪的修法」**注释**。与本文件顶部记的第一个判据坑同类，
已改为先剥注释（复用 `inspectableLines`）。**判据说「代码里别这么写」，就只能看代码。**
<br>⚠️ **注入验证踩了个新坑**：恢复注入时用了 `cp -p`（保留 mtime），
于是 cargo 认为源码未变、**沿用了注入版的编译产物**，全量跑出现一条假红。
`cp -p` 对 `tauri.conf.json` 那类「不想触发重建」的文件是对的，
对参与编译的 `.rs` 文件恰恰是错的 —— 恢复后要 `touch` 一下。

## 其他硬规则

- 改配置/二进制前必先备份（带日期后缀，可回滚）。
- 禁止把本机路径硬编码进代码；路径一律动态解析（面向通用用户）。**已有机械判据**，见上一节。
- 运行数据：`%APPDATA%\SynaRoute\{config.json, secrets.enc}`。

## 文档

- **[docs/19-开发文档.md](docs/19-开发文档.md) ← 全景：代码现在是什么样**
  （模块清单 / FR·NFR 逐条实现状态 / 四条「界面撒谎」/ 未做的事按可发布性分级 /
  质量门 / 发版验收判据 / 缺陷分类法）。**本文件答「为什么是这样、踩过什么坑」，
  它答「现在是什么样」** —— 两者互补，接手时都要读。
  <br>🔴 **勘误（2026-08-27）**：这里原先写「它记着两条明确未达标的 NFR：单文件 ≤900 行、
  单日日志 ≤100MB」—— **那两条 NFR 不存在**，docs/01 里 NFR-006 是「密钥加密与脱敏」、
  NFR-012 是「上手成本 5 分钟」，而 900/100MB 在 docs/01 与 docs/02 里都搜不到。
  更糟的是这构成**循环引用**：本行说「docs/19 记着」，docs/19 说「需求文档要求」，
  而需求文档里那条是别的内容。docs/19 §5 已按原文重建（**16 条**不是 12 条）。
  <br>两条的真实性质：900 行是 `ratchet.json` 的 `newFileCapLines`（**只管新文件、已在执行**，
  规则自洽，不存在「文档写 900、代码 3137」那种不一致）；100MB 没有出处，
  但**它指向的风险是真的**（当时全仓无任何按体积限日志的代码）——
  ✅ **已实现并发布**（v0.1.42）：`log_rotate.rs` 两级上限，见本文档「日志体积上限」那条。
  定的值是 256 MB/天，不是那个凭空的 100MB。
  <br>顺带更正：生产段超 900 行的是 **16 个**文件（旧版只数了 Rust，漏掉前端那几个），
  与 `ratchet.json` 的 `frozen` 条目数一致 —— 这个数字**由门自己打印**，
  2026-08-30 复核时发现文档里抄的「17」两处都是错的。
- [docs/01-需求规格说明书.md](docs/01-需求规格说明书.md)
  <br>⚠️ 每条只有一两句话、**没有验收判据**，故「FR-001~027 逐条核过」只说明功能点存在，
  不说明功能做对了 —— 本轮修的每个缺陷都不在那 27 条里。见 docs/19 §4 末。
- [docs/02-技术架构设计文档.md](docs/02-技术架构设计文档.md)
- [docs/03-UIUX设计文档.md](docs/03-UIUX设计文档.md)
- [docs/04-构建部署指南.md](docs/04-构建部署指南.md) ← 构建部署坑点与流程
- [docs/11-MSIX虚拟化踩坑复盘.md](docs/11-MSIX虚拟化踩坑复盘.md) ← 平行宇宙惨案完整复盘与逃逸手段
- [docs/12-CLI用户手册.md](docs/12-CLI用户手册.md) ← 面向试用用户的 Claude Code CLI 接入手册
- [docs/14-交接与待办清单.md](docs/14-交接与待办清单.md) ← **换机接手必读**：P0 待修缺陷 / 待办分级 / 审查边界 / 判据取证方法
  <br>🔴 **待办看第二十一节**（2026-08-27 落盘）：B~E 四层，**每条带精确代码坐标**
  （现状在哪几行、挂点在哪、已知的坑）。docs/19 §7 已改为只指向它 ——
  两处各写一份必然漂移，本仓吃过一次（两个门脚本各写一份「测试段起始行」判据并已分叉）。
  <br>§21.0 记着落这份清单时**修出的三处文档错误**，动手前先读那段。
- [docs/15-架构评审报告.md](docs/15-架构评审报告.md) ← **2026-08-03 全量架构评审**：20 条问题分级 + 优化方案 +
  效率/易用性清单 + 三批路线图 + **「不建议改动」清单（防重复劳动，动手前先查）**。
  第一批 9 项已实施（连接池/req_log 守卫/免克隆/并发丢计数/前端 selector），第二三批待办
