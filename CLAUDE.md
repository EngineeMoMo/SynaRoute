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
- **仍未做**：真机验证清单（docs/14 第八节，15 条）、提交与发 Release
- **大脑聚合多模态 + 工具调用已实现**（2026-08-01，FR-026 / FR-027，详见 docs/14 第一节末专节）：
  参与者可用一组**只读**工具（`read_file`/`grep`/`list_dir`/`codegraph_query`）按需检索，
  开关默认**关**（每轮重发完整历史，额度消耗显著更高）；MCP `images` 参数支持传图
  （相对 cwd、≤4 张、单张 ≤5MB、png/jpg/gif/webp）。
  **三道路径防线恒定生效**：拒 `..`/绝对路径 → canonicalize 后须仍在工作目录内 →
  凭据类文件一律拒读（按「模型给的名字」与「解析链接后的真实落点」**各判一次**，两次都不能省）。
  每条防线都做过故障注入验证（去掉后测试必须变红）。当前基线 **433 passed / 0 failed**
  <br>📌 **基线口径（2026-08-26 实测）**：上面各条历史行里的 311/312/368/383/433/506/719/760
  都是**当时**的数字，勿当现值。当前实测 `cargo test --lib` = **806 passed / 0 failed / 3 ignored**
  （连跑 3 次全绿；`cargo clippy --lib --all-targets -- -D warnings` 零警告；
  `tsc --noEmit` 干净；`npm test` 110 passed / 12 文件；`npm run build` 通过含颜色类零 CSS 检查；
  `npm run gates` 全绿）。
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

当前基线在 `config/quality/ratchet.json`：17 个冻结文件、新文件上限 900 行、
1 项计数（`bare_persist_calls: 3`，对上 docs/14 勘误里那条）。

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
