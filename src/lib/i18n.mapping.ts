// 模型映射区（档位快捷映射 + 精确映射 + 桌面端对外名体检）的本地化词条。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上、余量为 0）。粒度按**界面区块**分，
// 与 i18n.fields.ts / i18n.usage.ts 同一口径。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致，且**本文件必须在 src/lib/i18n.test.ts 的 SOURCES
// 与 CHUNKS 两张表里各有一条** —— 否则这一整页词条会静默脱离对称性保护
// （`usage.*` 那 32 条就这么脱管过一次）。

type Dict = Record<string, string>;

export const mappingZh: Dict = {
  // 桌面端对外模型名即时校验（UX#4）。不合规名会被桌面端**静默过滤**，全被过滤则选择器为空。
  "editor.desktopNameBadRow":
    "「{name}」会被 Claude 桌面端过滤掉：对外名须含 claude/opus/sonnet/haiku 之一，且不能含 glm/gpt/grok/deepseek 等厂商名。",
  "editor.desktopNameFixTo": "改为 {name}",
  "editor.desktopNameBadBanner":
    "{n} 个对外模型名不被 Claude 桌面端接受。桌面端加载配置时会把它们从模型列表里删掉——全被删完则模型选择器为空、打开会话报 ModelsNotDiscoveredError。",
  "editor.desktopNameFixAll": "一键加映射（{n} 条）",
  "editor.desktopNameFixAllHint":
    "给模型列表里每个模型各加一条映射：不合规的换成建议的合规对外名，其余保持原名。上游仍然请求真实模型名。",
  "editor.desktopNamePrefixUseless": "注意：claude-synaroute- 前缀对桌面端无效，厂商名黑名单优先。",
  "editor.comboNoModels": "先拉取或添加模型，或直接输入模型名",
  "editor.tierTitle": "档位快捷映射（快 / 中 / 强 / 最强）",
  "editor.tierHint":
    "Claude Code 按任务发不同档位模型，配好即自动改写为上游真实模型。留空则该档不生效，落到下方精确映射或兜底。",
  "editor.tierHaiku": "快 · Haiku",
  "editor.tierSonnet": "中 · Sonnet",
  "editor.tierOpus": "强 · Opus",
  "editor.tierFable": "最强 · Fable",
  "editor.mappingTitle": "模型映射（真实名 → 对外名 · 显示名）",
  "editor.addMapping": "添加映射",
  "editor.noMapping": "无映射时，真实模型名即对外名称",
  // 三列各自的表头。「对外名」这个词对用户没有意义，故一并说明它是什么。
  "editor.mappingColReal": "真实模型",
  "editor.mappingColOutward": "对外名（自动生成）",
  "editor.mappingColDisplay": "菜单显示名",
  "editor.mappingHint":
    "选好真实模型即可，对外名会自动生成一个客户端能接受的合规名。菜单显示名留空 = 显示真实模型名，这样你在 Claude 的模型菜单里看到的就是 glm-5.3 而不是那串合规名；实际请求始终发给真实模型。",
  // 🔴 如实告知副作用：填了显示名后，桌面端会把它写进系统提示词。
  // 隐瞒一个「你已经知道会发生」的行为，与承诺一件做不到的事同样糟。
  "editor.mappingDisplayNote":
    "填了显示名后，Claude 桌面端会在系统提示词里加一句「本部署把该模型标注为 …」——模型因此知道自己不是 Claude，通常是好事，但它确实改变了模型看到的上下文。",
};

export const mappingEn: Dict = {
  "editor.desktopNameBadRow":
    "Claude Desktop will drop “{name}”: an outward name must contain one of claude/opus/sonnet/haiku and must not contain a vendor name such as glm/gpt/grok/deepseek.",
  "editor.desktopNameFixTo": "Change to {name}",
  "editor.desktopNameBadBanner":
    "{n} outward model names are not accepted by Claude Desktop. It drops them from the model list when loading the config — if all of them are dropped, the model picker is empty and opening a chat throws ModelsNotDiscoveredError.",
  "editor.desktopNameFixAll": "Add mappings ({n})",
  "editor.desktopNameFixAllHint":
    "Adds one mapping for every model in the list: non-compliant ones get a suggested compliant outward name, the rest keep theirs. Requests still hit the real upstream model.",
  "editor.desktopNamePrefixUseless":
    "Note: the claude-synaroute- prefix does not help here — the vendor blocklist takes precedence.",
  "editor.comboNoModels": "Fetch or add models first, or just type a model name",
  "editor.tierTitle": "Quick tier mapping (fast / mid / strong / strongest)",
  "editor.tierHint":
    "Claude Code sends different tier models per task; set these and they're rewritten to the upstream real model. Leave empty to skip a tier and fall through to the exact mappings or fallback below.",
  "editor.tierHaiku": "Fast · Haiku",
  "editor.tierSonnet": "Mid · Sonnet",
  "editor.tierOpus": "Strong · Opus",
  "editor.tierFable": "Strongest · Fable",
  "editor.mappingTitle": "Model mapping (real → exposed · label)",
  "editor.addMapping": "Add mapping",
  "editor.noMapping": "Without mapping, the real model name is used as-is",
  "editor.mappingColReal": "Real model",
  "editor.mappingColOutward": "Exposed name (auto)",
  "editor.mappingColDisplay": "Menu label",
  "editor.mappingHint":
    "Just pick the real model — the exposed name is generated for you in a form the client accepts. Leave the menu label empty to show the real model name, so Claude's model picker shows glm-5.3 instead of that compliant alias; requests always go to the real model.",
  "editor.mappingDisplayNote":
    "With a label set, Claude Desktop appends “The administrator of this deployment has labeled this model …” to the system prompt — so the model knows it isn't Claude. Usually a good thing, but it does change what the model sees.",
};
