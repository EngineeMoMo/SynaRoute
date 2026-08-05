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
  "nav.features": "核心功能",
  "nav.screenshots": "产品截图",
  "nav.download": "下载",
  "nav.docs": "使用文档",
  "nav.changelog": "更新日志",
  "nav.github": "GitHub",

  // ---------- Hero ----------
  "hero.badge": "Windows 桌面软件 · 全部本地运行",
  "hero.title": "一个 Key 失效，下一个自动接上",
  "hero.desc":
    "SynaRoute 在你的电脑上架起一层本地 API 路由代理，为 Claude CLI、Claude 桌面端和 Codex 桌面端统一管理多个厂商的 API Key。主 Key 超时或报错时按优先级自动转移到下一个可用 Key，客户端无感知，不用改配置、不用重启。",
  "hero.descSecond": "密钥全程留在本机加密存储，没有云端账号，也不会上传任何配置。",
  "hero.ctaPrimary": "下载 Windows 版",
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
  "features.title": "核心功能",
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
  "download.fallbackNote": "无法从 GitHub 获取最新版本信息，以下为已知版本。你也可以直接前往发布页查看。",
  "download.allReleases": "查看全部历史版本",
  "download.verifyTitle": "关于安装时的系统提示",
  "download.verifyDesc":
    "安装包目前没有代码签名证书，Windows SmartScreen 可能提示「未知发布者」。如果介意，可以在 GitHub 发布页核对文件大小后再安装。",
  "download.updateTitle": "关于更新",
  "download.updateDesc": "软件内置更新检查，有新版本时会在界面上提示，无需手动来官网重新下载。",

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

  "faq.q1": "这个软件收费吗？",
  "faq.a1": "免费。不需要注册账号，没有付费功能，也没有使用次数限制。你只需要自备各厂商的 API Key。",

  "faq.q2": "支持哪些操作系统？",
  "faq.a2":
    "目前只有 Windows 版（Windows 10 1809 及以上）。macOS 版本正在开发中，做好后会在这里提供下载。暂无 Linux 版本计划。",

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
  "cta.title": "让多个 Key 自己接力",
  "cta.desc": "免费使用，全部本地运行，不需要注册账号。",

  // ---------- 页脚 ----------
  "footer.tagline": "面向 Claude CLI、Claude 桌面端与 Codex 桌面端的本地 API Key 路由代理。",
  "footer.product": "产品",
  "footer.resources": "资源",
  "footer.legal": "条款",
  "footer.contact": "联系",
  "footer.privacy": "隐私政策",
  "footer.terms": "用户协议",
  "footer.email": "邮箱",
  "footer.authorSite": "作者主页",
  "footer.copyright": "© {year} SynaRoute. 保留所有权利。",
  "footer.sourceNote": "源码公开在 GitHub（尚未附带开源许可证）。",

  // ---------- 平台 ----------
  "platform.windows.name": "Windows",
  "platform.macos.name": "macOS",
  "platform.linux.name": "Linux",

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
