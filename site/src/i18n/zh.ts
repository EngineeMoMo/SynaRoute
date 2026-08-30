import type { Dict } from "./index";

/**
 * 中文文案（基准语言）。
 *
 * 内容准则（对应需求模板第 24 节「禁止事项」）：
 * - 只写代码里能取证的事实：版本、路径、加密方式、支持平台都有出处。
 * - 不写「绝对安全 / 100% 安全」，不虚构安全审计与隐私认证。
 * - 不虚构用户数、下载量、媒体报道、合作伙伴。
 * - 仓库是 public 但**没有 LICENSE 文件**，因此一律表述为「源码公开」，不称「开源」。
 */
export const zh: Dict = {
  // ---------- 通用 ----------
  "common.download": "立即下载",
  "common.viewGithub": "查看 GitHub",
  "common.docs": "使用文档",
  "common.comingSoon": "即将推出",
  "common.copy": "复制",
  "common.copied": "已复制",
  "common.close": "关闭",
  "common.backHome": "返回首页",
  "common.loading": "加载中…",
  "common.retry": "重试",
  "common.openInGithub": "在 GitHub 上查看",
  "common.skipToContent": "跳到主要内容",
  "common.backToTop": "返回顶部",
  "common.toggleTheme": "切换深色/浅色模式",
  "common.toggleLang": "切换语言",
  "common.openMenu": "打开菜单",
  "common.closeMenu": "关闭菜单",

  // ---------- 导航 ----------
  "nav.home": "首页",
  "nav.brain": "大脑聚合",
  "nav.features": "功能",
  "nav.screenshots": "产品截图",
  "nav.download": "下载",
  "nav.docs": "使用文档",
  "nav.changelog": "更新日志",
  "nav.github": "GitHub",

  // ---------- Hero ----------
  // ⚠️ 这条原先写的是「Windows 桌面软件」，而 v0.1.33 起 macOS 与 Linux 都已发布
  // （`data/platforms.ts` 三条全是 available，下载区就摆着三张卡）——
  // 首屏第一句话与同页下载区互相矛盾。平台清单交给下方的 PlatformBadges，
  // 这条徽标只说「是桌面软件、在本机跑、不用注册」这三件都能取证的事。
  "hero.badge": "桌面软件 · 全部本地运行 · 无需注册账号",
  // 标题拆两段：中文两段各自套 whitespace-nowrap 锁成整体，避免窄屏在
  // 「Key 互为备份」中间断行。切分点就是那个逗号，改文案时保持这个结构。
  "hero.titleLead": "多个 Key 互为备份，",
  "hero.titleTail": "多个模型协同思考",
  "hero.desc":
    "本机运行的 API 路由代理，为 Claude CLI、Claude 桌面端和 Codex 桌面端统管多家厂商的 Key 与模型。",
  "hero.descSecond":
    "主 Key 报错自动换下一个；也能让多个模型并行回答同一个问题，再由决策者综合出结论。",
  "hero.ctaPrimary": "下载 Windows 版",
  // 三个平台都已发布（v0.1.33 起同时出 exe / dmg×2 / AppImage+deb+rpm），
  // 故主 CTA 按访客所在平台取词，而不是恒写「Windows 版」。
  "hero.ctaFor": "下载 {platform} 版",
  "hero.ctaWindows": "下载 Windows 版",
  "hero.ctaOtherPlatforms": "其它平台与架构",
  // 认错平台而没有出路 = 用户下到装不上的包，故这一条必须显眼。
  "hero.ctaPickPlatform": "没认出你的系统？手动选择平台",
  "hero.ctaPrimaryMacHint": "macOS 版即将推出",
  "hero.ctaSecondary": "查看 GitHub",
  "hero.versionPrefix": "当前版本",
  "hero.screenshotAlt": "SynaRoute 主界面：Claude CLI 分类下的多个厂商 Key 列表，含优先级排序、健康状态与代理端点",

  // ---------- 核心优势 ----------
  "benefits.title": "为什么需要它",
  "benefits.subtitle": "多 Key 用户每天都在手动做的事，交给一个常驻程序去做。",

  "benefits.failover.name": "失败自动接力",
  "benefits.failover.desc": "主 Key 报错、超时或触发限流时，按你排好的优先级顺次切换到下一个可用 Key。",
  "benefits.failover.more":
    "切换前会先做一次健康探测，避免切到同样不可用的 Key；连续失败的 Key 会被临时摘除，恢复后自动放回。",

  "benefits.threeClients.name": "三个客户端分开管",
  "benefits.threeClients.desc": "Claude CLI、Claude 桌面端、Codex 桌面端各有独立分类，配置互不干扰。",
  "benefits.threeClients.more":
    "每个分类有自己的 Key 列表、主 Key、代理端口与模型映射。给 CLI 换 Key 不会动到桌面端。",

  "benefits.local.name": "密钥不出本机",
  "benefits.local.desc": "配置与密钥都存在本地文件里，没有云端账号，不做配置同步，也不收集使用数据。",
  "benefits.local.more": "密钥经 Windows 数据保护接口（DPAPI）加密后落盘，另可选开启主口令做二次加密。",

  "benefits.protocol.name": "协议自动转换",
  "benefits.protocol.desc": "客户端说的和厂商听的不是一种协议时，由代理层在中间做转换。",
  "benefits.protocol.more":
    "支持 Anthropic Messages、OpenAI Chat Completions、OpenAI Responses 三种协议互转，流式与非流式都覆盖。",

  // ---------- 核心功能 ----------
  "features.title": "其他功能",
  "features.subtitle": "围绕「多 Key、多客户端、多协议」这三件事展开。",

  "features.failover.name": "故障转移路由",
  "features.failover.short": "主 Key 出问题时，请求自动落到下一个可用 Key。",
  "features.failover.desc":
    "Key 按列表顺序构成优先级，转移时从上往下找第一个可用的。切换前做健康探测，连续失败的 Key 进入短路窗口被临时跳过，窗口结束后自动重新参与。上游返回的限流等待时间会被透传给客户端，而不是被吞掉。",

  "features.mapping.name": "模型映射",
  "features.mapping.short": "把厂商的真实模型名，映射成客户端认识的名字。",
  "features.mapping.desc":
    "第三方厂商的模型名往往和客户端期望的对不上。配置一条映射后，客户端仍按熟悉的名字调用，代理层负责翻译成厂商真实模型名。还可以配置兜底模型，用于候选 Key 不提供所请求模型时的降级。",

  "features.brain.name": "大脑聚合",
  "features.brain.short": "多个模型并行解答同一个问题，再由决策者产出最终答案。",
  "features.brain.desc":
    "配置若干「成员」（Key + 模型的组合）并行回答，然后交给指定的决策模型汇总。汇总方式可选压缩汇总或全量上下文。成员还可以按需读取工作目录里的文件作为参考，也支持传图片作为输入。",

  "features.protocol.name": "跨协议转换",
  "features.protocol.short": "任意客户端协议对任意上游协议，动态转换。",
  "features.protocol.desc":
    "Anthropic Messages、OpenAI Chat Completions、OpenAI Responses 三种协议之间双向转换，覆盖流式与非流式、工具调用与多轮历史。这也意味着故障转移可以跨协议进行——主 Key 和备用 Key 不必是同一家厂商。",

  "features.secret.name": "加密密钥存储",
  "features.secret.short": "密钥加密后存在本地文件，可选主口令二次保护。",
  "features.secret.desc":
    "默认使用 Windows 数据保护接口（DPAPI）加密，密文与当前 Windows 账户绑定，换机器或换账户无法解出。可选开启主口令增强模式，改用口令派生密钥（Argon2id）配合 AES-GCM 加密，此时启动需要先解锁。",

  "features.apply.name": "一键接入客户端",
  "features.apply.short": "点「启动」，自动把代理端点写进客户端配置。",
  "features.apply.desc":
    "启动代理时自动写入对应客户端的配置文件，停止时自动还原，并在写入前备份原配置。三个客户端各写各的字段，互不覆盖。界面里可以先预览将要写入的内容再决定。",

  "features.logs.name": "运行日志",
  "features.logs.short": "每次转发发生了什么，都能查。",
  "features.logs.desc":
    "记录转发时间、命中哪个 Key、请求的模型被解析成了什么、上游返回什么状态、是否发生了转移。支持搜索与导出诊断报告。记录完整对话正文的开关默认关闭，需要排障时才临时打开。",

  "features.portable.name": "配置导入导出",
  "features.portable.short": "整套配置可打包带走，支持合并或覆盖导入。",
  "features.portable.desc":
    "导出的文件带校验值。因为密钥密文与本机账户绑定、换机器解不出，含密钥的导出会用你设定的口令重新加密。端口、日志目录这类本机运行态不随导出走。",

  "features.tray.name": "托盘与自启动",
  "features.tray.short": "常驻托盘，可快速启停代理与切换主 Key。",
  "features.tray.desc":
    "托盘图标按代理运行状态变化。右键菜单可分别启停三个分类的代理、快速切换主 Key。可选开机自启动，随系统启动时最小化到托盘。",

  // ---------- 下面四条是 0.1.30~0.1.33 新增的能力 ----------
  // 官网原先一个字都没提，而它们恰恰是目标用户（开发者）最会买单的部分。
  // 每一条都只写软件真的能做的事，并把代价一并写出来。

  "features.diag.name": "排障响应头",
  "features.diag.short": "每个回答都带一行可粘贴的路由信息：走了哪条 Key、切了几次。",
  "features.diag.desc":
    "响应头里带 X-SynaRoute-Decision（key / model / attempts / latency 一行）以及请求 ID、上游状态码、版本号。出问题时不必再猜「刚才那次是走哪条 Key 的」，把这一行贴出来就够了。头里刻意不放地址与密钥 —— 有些中转站把令牌放在 URL 路径里，回显地址等于把密钥回显给客户端。",

  "features.resilience.name": "单模型锁定",
  "features.resilience.short": "某个模型不可用时只挡那一个，不再连坐整条 Key。",
  "features.resilience.desc":
    "中转站最常见的失败是「这条 Key 的某个模型没开通」，对它服务的其它模型完全正常。此前这类失败三次就把整条 Key 停用一分钟，把好模型一起误伤。现在按模型单独退避，锁的是上游真实模型名（锁对外名换个别名就绕过去了）。同一条 Key 上锁了三个模型才升级为整条熔断 —— 否则一条「什么都不通」的 Key 会永远赖在候选池首位。",

  "features.usage.name": "用量与花费统计",
  "features.usage.short": "按分类与 Key 统计 token，并按内置单价表估算花费。",
  "features.usage.desc":
    "内置各厂商在役模型的单价表（含缓存命中价逐厂商不同的那部分），按 Key 估算花费；中转站有折扣时填一个计费倍率即可校准。算不出金额时会说明为什么算不出，而不是显示 0 —— 界面上还标着单价表的核对日期，让你自己判断这个估算值得信几分。",

  "features.balance.name": "余额查询",
  "features.balance.short": "直接在 Key 卡片上看中转站还剩多少额度。",
  "features.balance.desc":
    "认得出的站点零配置就能查（按域名匹配端点，NewAPI 系还会现场读取它自己的计费单位换算比率）；认不出的站点会按序探测几个常见端点并把命中的那个记下来。查不到就如实说「这个站点没有余额接口」，不拿配额上限之类的数字冒充余额 —— 那会给出一个你会当真的错数字。",

  // ---------- 下面两条补齐 0.1.41~0.1.42 的能力 ----------
  // 加它们同时解决一个排版约束：`third` 必须是 6 的倍数（sm 两列与 lg 三列都要排满），
  // 原先 10 条会在桌面端留一张孤卡。详见 data/features.ts 的注释。
  "features.lan.name": "局域网共享",
  "features.lan.short": "可选让同网段的设备也用这台机器的代理，非本机必须带令牌。",
  "features.lan.desc":
    "默认只监听本机回环地址。开启后同网段设备也能连，但非本机来源一律要校验接入令牌才放行 —— 令牌在设置页里查看、复制、重新生成，日志与诊断报告里只留前 8 位指纹，避免随手贴出的日志把额度送人。监听同时绑 IPv4 与 IPv6，不会出现「有的机器连得上、有的连不上」。",

  "features.codexModels.name": "Codex 模型选择器",
  "features.codexModels.short": "让非 GPT 模型出现在 Codex 自己的模型菜单里。",
  "features.codexModels.desc":
    "接入 Codex 时会顺带写一份模型目录给它读，于是你配的模型（包括 Claude、GLM 这类非 GPT 的名字）会直接出现在它的模型菜单里，并带上思考强度档位。档位按上游协议推导：只有「思考开关」而没有档位的厂商刻意不声明 —— 声明了在 Codex 里调也不会有任何效果。改完模型列表要重开一次 Codex 才生效。",

  // ---------- 大脑聚合专区 ----------
  // 只写软件真的能做的事。「决策者必填」「工具调用默认关且更耗额度」这两条
  // 是刻意写进来的：先说清代价，用户装了之后不会觉得被夸大宣传骗了。
  "brain.badge": "特色能力",
  "brain.title": "让多个模型一起想，再由一个模型拍板",
  "brain.subtitle":
    "同一个问题交给多个模型并行回答，再由你指定的决策模型综合出最终结论。适合代码审查、方案设计、疑难排查这类单个模型容易漏视角的任务。",

  "brain.flow.members": "成员并行回答",
  "brain.flow.membersHint": "2～4 个 Key + 模型的组合，同时开跑",
  "brain.flow.merge": "汇总",
  "brain.flow.mergeHint": "压缩汇总省额度，或全量上下文保信息",
  "brain.flow.decider": "决策者产出结论",
  "brain.flow.deciderHint": "必填，建议选能力最强的那个模型",

  "brain.cap.strategy.title": "两种汇总策略",
  "brain.cap.strategy.desc":
    "压缩汇总先让汇总模型把各家答案精简，成员多时更省额度；全量上下文把原答案整份交给决策者，信息最全。并发上限与单成员超时都可调。",
  "brain.cap.retrieval.title": "按需读你的代码",
  "brain.cap.retrieval.desc":
    "可开启一组只读工具，让成员自己决定读哪个文件、搜什么关键词、看哪个符号。工具永不写文件、不执行命令，且限制在工作目录内。默认关闭——每轮都要重发完整历史，额度消耗明显更高。",
  "brain.cap.images.title": "支持传图",
  "brain.cap.images.desc":
    "报错截图、界面稿都可以作为输入。最多 4 张、单张不超过 5MB。任何一张不合规会整次报错说明原因，不会静默丢掉某张图让你拿到一个「其实没看图」的答案。",
  "brain.cap.mcp.title": "也能当 MCP 工具用",
  "brain.cap.mcp.desc":
    "开启 MCP 服务器后，Codex CLI 与 Claude Code 可以直接调用它。这条通道只返回建议、绝不改你的文件，所有修改仍由你的客户端执行。",

  "brain.ctaDocs": "看使用说明",
  "brain.ctaMcp": "作为 MCP 工具接入",
  "brain.screenshotAlt": "大脑聚合配置界面：参与成员列表、最终决策者、聚合策略与并发超时设置",

  // ---------- 截图 ----------
  "screenshots.title": "界面一览",
  "screenshots.subtitle": "以下截图取自软件的浏览器预览模式，数据为示例数据。点击可放大。",
  "screenshots.enlarge": "点击放大",
  "screenshots.placeholder": "产品截图占位（图片暂未提供）",
  "screenshots.prev": "上一张",
  "screenshots.next": "下一张",

  "screenshots.category.title": "Key 管理",
  "screenshots.category.desc": "卡片式列表，可拖动优先级、切换启用状态、查看健康探测结果与代理端点。",
  "screenshots.brain.title": "大脑聚合",
  "screenshots.brain.desc": "配置并行解答的成员、决策模型与汇总方式。",
  "screenshots.logs.title": "运行日志",
  "screenshots.logs.desc": "每次转发的命中 Key、模型解析结果与上游状态，可搜索、可展开细节。",
  "screenshots.settings.title": "设置",
  "screenshots.settings.desc": "主题、语言、端口、日志与安全相关开关，每个开关都写明了代价。",
  "screenshots.vendors.title": "厂商管理",
  "screenshots.vendors.desc": "维护常用厂商的地址与协议类型，新增 Key 时直接选用。",

  // ---------- 下载 ----------
  "download.title": "下载",
  "download.subtitle": "免费使用，无需注册账号。",
  "download.pageTitle": "下载 SynaRoute",
  "download.version": "版本",
  "download.minOS": "系统要求",
  "download.format": "安装包格式",
  "download.size": "大小",
  "download.button": "下载",
  "download.buttonComingSoon": "即将推出",
  "download.recommended": "推荐给你",
  "download.macNote": "macOS 版本正在开发中，目前尚未提供下载。",
  "download.linuxNote": "暂无 Linux 版本计划。",
  "download.otherBuilds": "其它架构 / 格式：",
  "download.fallbackNote": "无法从 GitHub 获取最新版本信息，以下为已知版本。你也可以直接前往发布页查看。",
  "download.allReleases": "查看全部历史版本",
  "download.verifyTitle": "关于安装时的系统提示",
  "download.verifyDesc":
    "安装包目前没有代码签名证书，Windows SmartScreen 可能提示「未知发布者」。如果介意，可以在 GitHub 发布页核对文件大小后再安装。",
  "download.updateTitle": "关于更新",
  "download.updateDesc": "软件内置更新检查，有新版本时会在界面上提示，无需手动来官网重新下载。",
  // 上面那句对停在 v0.1.23 及更早的用户**是假的** —— 他们的应用内更新已永久失效,
  // 而软件里那句「已是最新」或验签失败提示不会说明该怎么办。这是唯一能触达他们的渠道
  // （他们收不到任何更新，也就看不到我们在新版里改好的错误文案）。
  "download.updateLegacyTitle": "如果你在用 v0.1.23 或更早的版本",
  "download.updateLegacyDesc":
    "这些版本的应用内更新已经失效：更新签名密钥更换过，旧版本内嵌的公钥无法验证新版本的签名，" +
    "所以它会一直提示校验失败或停在旧版本，重试多少次都一样。请在本页手动下载安装包，覆盖安装一次即可恢复正常更新。" +
    "配置、Key 和用量记录都存在系统的应用数据目录里，重装不会丢。",

  // ---------- 使用步骤 ----------
  "steps.title": "四步开始使用",
  "steps.subtitle": "不用读完整份文档也能跑起来。",

  "steps.s1.title": "下载并安装",
  "steps.s1.desc": "下载安装包，按向导完成安装。首次启动会自动创建本地配置目录。",
  "steps.s2.title": "添加厂商 Key",
  "steps.s2.desc":
    "选择要配置的客户端分类，新增一条 Key，填入厂商地址与密钥。保存时会自动拉取该厂商的可用模型列表并做一次健康检查。",
  "steps.s3.title": "点「启动」",
  "steps.s3.desc":
    "启动本地代理，SynaRoute 会自动把代理端点写入对应客户端的配置文件（写入前先备份原文件）。",
  "steps.s4.title": "照常使用",
  "steps.s4.desc":
    "回到 Claude Code 或 Codex 正常使用即可。请求经由本地代理转发，Key 出问题时自动接力，你不会感知到切换。",

  // ---------- 安全与隐私 ----------
  "security.title": "数据与隐私",
  "security.subtitle": "以下都是软件的实际行为，可以对照源码核对。",

  "security.storage.title": "数据存在哪里",
  "security.storage.desc":
    "全部在本机。配置文件与加密后的密钥文件位于当前用户的应用数据目录下的 SynaRoute 文件夹中。没有服务器端存储。",

  "security.encryption.title": "密钥如何保存",
  "security.encryption.desc":
    "默认经 Windows 数据保护接口（DPAPI）加密后落盘，密文与当前 Windows 账户绑定，复制到别的机器或别的账户下无法解出。可选开启主口令增强模式，改用 Argon2id 从口令派生密钥、配合 AES-GCM 加密。",

  "security.network.title": "会往外发什么",
  "security.network.desc":
    "只有你自己配置的上游厂商请求，以及检查新版本时对 GitHub 发布页的请求。不收集使用数据，不做行为统计，没有账号体系，也不做配置同步。",

  "security.logs.title": "关于日志",
  "security.logs.desc":
    "运行日志默认只记录元信息（时间、命中的 Key、模型、上游状态码）。「记录调用模型日志」开关默认关闭，开启后日志会包含完整对话正文（含系统提示词），仅建议排障时临时开启，查完关掉。日志只写在本地。",

  "security.delete.title": "如何彻底删除数据",
  "security.delete.desc":
    "卸载软件后，删除用户应用数据目录下的 SynaRoute 文件夹即可清除全部配置与密钥。软件不会在其他位置留存数据。",

  "security.source.title": "源码",
  "security.source.desc":
    "源码公开在 GitHub，上述行为都可以自行核对。请注意本项目目前尚未附带开源许可证。",

  "security.risk.title": "需要你注意的风险",
  "security.risk.desc":
    "本地代理监听在回环地址上，同一台机器上的其他程序理论上可以访问该端点。请不要把代理端口暴露到公网。另外，DPAPI 加密防的是文件被复制走，不防已经登录了你账户的程序。",

  // ---------- FAQ ----------
  "faq.title": "常见问题",
  // 七个用 SectionTitle 的区块里，FAQ 原先是唯一没有引导句的 —— 标题下面
  // 空着 48px 直接接手风琴，与其余六块的节奏不一致。
  "faq.subtitle": "下面十条是最常被问到的。答案都对应软件的实际行为。",

  "faq.q1": "这个软件收费吗？",
  "faq.a1": "免费。不需要注册账号，没有付费功能，也没有使用次数限制。你只需要自备各厂商的 API Key。",

  "faq.q2": "支持哪些操作系统？",
  // ⚠️ 这条原先写「目前只有 Windows 版……macOS 正在开发中……暂无 Linux 计划」，
  // 而 v0.1.33 起三个平台同时出包（data/platforms.ts 三条全是 available），
  // 同一页的下载区就摆着三张可下载的卡 —— 自相矛盾。按实际发布物重写。
  "faq.a2":
    "Windows、macOS、Linux 都有。Windows 10 (1809) 及以上；macOS 11+，Apple 芯片与 Intel 各一个 dmg；Linux 提供 AppImage、deb、rpm 三种包，需要 glibc 2.31+。下载区会按你的系统高亮推荐的那一个，其它平台与架构始终并列可见。",

  "faq.q3": "我的数据和密钥存在哪里？",
  "faq.a3":
    "全部在本机，位于当前用户应用数据目录下的 SynaRoute 文件夹。密钥经加密后存放，不会上传到任何服务器。",

  "faq.q4": "需要登录账号吗？",
  "faq.a4": "不需要。软件没有账号体系，也不做多设备配置同步。",

  "faq.q5": "怎么升级到新版本？",
  "faq.a5":
    "软件内置更新检查，发现新版本会在界面上提示并可直接更新。你也可以随时从 GitHub 发布页手动下载新版安装包覆盖安装。",

  "faq.q6": "遇到问题怎么反馈？",
  "faq.a6":
    "在 GitHub 提交 Issue，或发邮件给作者。反馈时附上软件内「导出诊断报告」生成的文件会更容易定位问题——该报告不含密钥明文。",

  "faq.q7": "支持自动更新吗？",
  "faq.a7": "支持。软件会检查发布页上的新版本并提示更新，更新包带签名校验。",

  "faq.q8": "源码公开吗？",
  "faq.a8":
    "源码公开在 GitHub 上，可以自行查阅和构建。需要说明的是，项目目前还没有附带开源许可证，因此严格来说不属于「开源软件」。",

  "faq.q9": "它会不会拿到我的 API Key？",
  "faq.a9":
    "Key 由你自己填写、加密存放在你的电脑上，只在转发请求时用于向你配置的厂商地址发起调用。软件没有把密钥发往作者或任何第三方的代码路径，你可以在源码里核对这一点。",

  "faq.q10": "和直接改客户端配置文件比，有什么区别？",
  "faq.a10":
    "手动改配置一次只能指向一个厂商，出问题要人工发现并再改一次。SynaRoute 让请求先经过本地代理，由代理在多个 Key 之间自动接力，同时保留可视化管理、健康检查与运行日志。",

  // ---------- 底部 CTA ----------
  // 标题拆两段，理由同 hero.titleLead/Tail：中文没有词边界，390px 下整句会断成
  // 「让多／个模型一起想」这种半个词。两段各自套 whitespace-nowrap 即可。
  "cta.titleLead": "让多个 Key 互为备份，",
  "cta.titleTail": "让多个模型一起想",
  "cta.desc": "免费使用，全部本地运行，不需要注册账号。",

  // ---------- 页脚 ----------
  "footer.tagline": "面向 Claude CLI、Claude 桌面端与 Codex 桌面端的本地 API 路由代理：多 Key 互为备份，多模型协同作答。",
  "footer.product": "产品",
  "footer.resources": "资源",
  "footer.legal": "条款",
  "footer.contact": "联系",
  "footer.privacy": "隐私政策",
  "footer.terms": "用户协议",
  "footer.email": "邮箱",
  "footer.authorSite": "作者 @{name}",
  "footer.copyright": "© {year} SynaRoute. 保留所有权利。",
  "footer.sourceNote": "源码公开在 GitHub（尚未附带开源许可证）。",

  // ---------- 平台 ----------
  "platform.windows.name": "Windows",
  "platform.windows.format": "exe · NSIS 安装包",
  "platform.macos.name": "macOS",
  "platform.macos.format": "dmg",
  "platform.linux.name": "Linux",
  "platform.linux.format": "AppImage",

  // ---------- 文档 ----------
  "docs.title": "使用文档",
  "docs.subtitle": "从安装到接入的完整说明。",
  "docs.onThisPage": "本页目录",
  "docs.backToDocs": "返回文档索引",
  "docs.editOnGithub": "在 GitHub 上查看原文",
  "docs.notFound": "没有找到这篇文档。",

  "docs.cli.title": "Claude CLI 接入手册",
  "docs.cli.desc": "从安装到跑通的完整步骤，含配置写入说明、模型选择规则与常见问题。",
  "docs.brain.title": "大脑聚合使用说明",
  "docs.brain.desc": "配置多个模型并行解答、汇总与最终决策，含参数含义与排查思路。",
  "docs.mcp.title": "MCP 接入手册",
  "docs.mcp.desc": "把大脑聚合作为 MCP 工具接入 Codex CLI 与 Claude Code。",

  // ---------- 更新日志 ----------
  "changelog.title": "更新日志",
  "changelog.subtitle": "版本记录来自 GitHub 发布页。",
  "changelog.loadFailed": "无法从 GitHub 加载更新日志。",
  "changelog.loadFailedHint": "可能是网络问题或接口调用频率限制，你可以直接前往发布页查看。",
  "changelog.empty": "暂无发布记录。",
  // 剥掉每个版本都重复的样板文字后确实没有正文的版本（只发了包、没写说明）。
  // 用一句话而不是一个「—」：破折号看起来像加载失败。
  "changelog.noNotes": "这个版本没有单独写发布说明。",
  "changelog.viewOnGithub": "在 GitHub 上查看这个版本",

  // ---------- 隐私政策 ----------
  "privacy.title": "隐私政策",
  "privacy.updated": "最后更新：{date}",

  // ---------- 用户协议 ----------
  "terms.title": "用户协议",
  "terms.updated": "最后更新：{date}",

  // ---------- 404 ----------
  "notFound.title": "页面不存在",
  "notFound.desc": "你访问的地址没有对应的页面，可能是链接过期或输入有误。",
};
