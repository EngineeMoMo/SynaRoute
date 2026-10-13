// 用量统计页的本地化词条。
//
// 从 i18n.ts 拆出来的：那个文件冻结在棘轮上、余量为 0，而本轮要给「为什么算不出花费」
// 的四种成因各补一条文案。拆的粒度按**页面**而不是按字母，因为改一个页面的文案时
// 要同时看 zh/en 两份，同页放在一起最省事。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致 —— 由 tests/i18nUsageParity.test.ts 机械校验。
// 漏一条 en 不会报错，只会让英文界面显示中文（静默降级）。

type Dict = Record<string, string>;
export const usageZh: Dict = {
  "usage.title": "用量统计",
  "usage.subtitle": "按分类与 Key 聚合的 token 消耗与花费估算（跨重启累计）",
  "usage.refresh": "刷新",
  "usage.refreshing": "刷新中…",
  "usage.loading": "加载中…",
  // 空态改口径：配置了 Key 之后表格就不再是空的（它们会以「尚无用量」的形态出现），
  // 所以真正的空态只剩「一条 Key 都没配、也没有历史用量」。旧文案「有转发流量后
  // 这里会出现数据」在那种情形下会让用户以为得先跑一次请求才能看到自己的 Key。
  "usage.empty": "还没有配置 Key，也没有历史用量记录",
  "usage.input": "输入",
  "usage.output": "输出",
  "usage.cacheRead": "缓存读取",
  "usage.total": "合计",
  "usage.colCategory": "分类",
  "usage.colKey": "Key",
  "usage.systemLevel": "（系统级）",
  "usage.since": "统计起点",
  "usage.sinceHint": "每分钟自动保存一次（仅在有新消耗时写盘），意外退出最多丢 1 分钟",
  // 金额与趋势。前三格是**日历**口径（本周从周一、本月从 1 号，本机时区），主数字是花费，
  // token 数作副行 —— 标签与数字的单位必须一致，否则「今日 1.2M」会被读成钱。
  "usage.today": "今日花费",
  "usage.thisWeek": "本周花费",
  "usage.thisMonth": "本月花费",
  "usage.periodTokens": "{tokens} token",
  "usage.periodHint": "按本机时区的日历统计（本周从周一算起，本月从 1 号算起）。金额是估算，不是账单。",
  "usage.periodUnpriced": "另有 {tokens} token 无法估价，未计入",
  "usage.estCost": "累计花费（估算）",
  "usage.trend7d": "近 7 日用量",
  "usage.trendTip": "{date} · {tokens} token · 约 {cost}",
  "usage.trendTipPartial": "{date} · {tokens} token · 约 {cost}（另有 {unpriced} token 无法估价）",
  "usage.colCost": "花费",
  "usage.estimateHint":
    "按官方单价（内置表 + 每天更新的公开价格表）× 你填的计费倍率估算，不是账单。每个模型按自己的单价算；中转站实际计价可能不同，可在 Key 编辑器里调整「计费倍率」校准。",
  "usage.costExactHint": "按官方单价表计算，倍率 {multiplier}",
  "usage.costFamilyHint":
    "有一部分是估算的（标 ≈）：升级前没记模型的历史用量按这条 Key 的代表模型估，或模型不在表里、按家族名估。倍率 {multiplier}，偏差可能较大。",
  "usage.breakdownTitle": "按模型拆分：",
  "usage.breakdownLegacy": "升级前的用量（未记模型，按 {model} 估）",
  "usage.breakdownLegacyUnknown": "升级前的用量（未记模型）",
  "usage.breakdownUnpriced": "无法估价",
  "usage.partialMark": "+?",
  "usage.partialHint": "这一行有一部分用量算不出金额，上面的数字只含能算的那部分。",
  "usage.costUnknownHint":
    "无法估算：这条 Key 没有可识别的模型名。在 Key 里设置「默认兜底模型」或拉取模型列表后即可估算。",

  // ---- 「为什么算不出花费」的四种成因 ----
  //
  // 旧实现把四种成因压成同一个「—」，只给一句放之四海的提示：「这条 Key 没有可识别的
  // 模型名。在 Key 里设置默认兜底模型…」。它**只对第四种成立**，对另外三种都是把用户
  // 送去做无效操作 —— 而做完之后界面上没有任何新信息，用户只能认为功能坏了。
  "usage.reason.aggregate":
    "这笔是「大脑聚合」的消耗，旧版本没把它记到具体 Key 上，因此取不到单价与倍率。升级后新产生的聚合用量会归到各参与者 Key 上；这一行是历史数据，会停止增长但不会消失。",
  "usage.reason.keyDeleted":
    "这一行的 Key 已不在配置里，且当时没有留下它的代表模型与计费倍率，所以升级前没记模型的那部分用量算不出金额。token 数仍计入总量；不需要这一行的话可以点「清理」删掉。",
  "usage.keyDeletedTag": "（已删除）",
  "usage.reason.noModelName":
    "升级前的用量没有记下模型，而这条 Key 也没有可用来估价的模型名。在 Key 编辑器里设置「默认兜底模型」或拉取一次模型列表即可估算。",
  "usage.reason.modelNotInTable":
    "模型「{model}」不在单价表里（{tables}），这部分用量没有计入金额。可能是还没收录的新模型，或中转站自定义的名字。",
  "usage.reason.tablesBoth": "内置表核对于 {date}，在线表 {n} 个模型也没有它",
  "usage.reason.tablesBuiltinOnly": "内置表核对于 {date}；在线表还没拉到",
  // 横幅按成因分组，每组一句、各自指路。
  "usage.unpricedBanner": "有 {n} 行的用量无法（全部）估算花费：",
  "usage.unpricedGroup.aggregate": "{n} 行是旧版大脑聚合用量（无 Key 归属）",
  "usage.unpricedGroup.keyDeleted": "{n} 行的 Key 已不在配置里且没有留档 —— 不需要的话可在表格里点「清理」",
  "usage.unpricedGroup.noModelName": "{n} 行没有代表模型名 —— 去 Key 里设「默认兜底模型」即可",
  "usage.unpricedGroup.modelNotInTable": "{n} 行的模型不在单价表：{models}",
  // 累计金额那一格：**必须**标出「有几行没算进去」。
  // 只在下方横幅里说是不够的 —— 那个数字自己标着「累计花费」，用户会当成总计。
  "usage.estCostExcluded": "＋{n} 行未（全部）计入",
  "usage.pricedBy": "主要按 {model} 估算",
  // ---- 清理已删除 Key 的用量 ----（不可逆；用删除按钮删的 Key 会自动一并删掉，不会出现在这里）
  "usage.purge": "清理",
  "usage.purgeHint": "删除这条已不在配置里的 Key 的全部用量记录（不可恢复）",
  "usage.purgeConfirm": "确认清理",
  "usage.purgeNotPersisted": "已从列表移除，但用量文件这次没有写成（只读或被占用），重启后可能重新出现。",
  "usage.deleteKeyNote": "这条 Key 的用量记录也会一并删除，不可恢复。",
  // ---- 单价表现状（表头一行；在线表 = LiteLLM 公开价格，后端每天拉一次）----
  "usage.priceBuiltin": "内置单价表核对于 {date}",
  "usage.priceOnline": "在线价格 {n} 个模型（{time} 更新）",
  "usage.priceOnlineStale": "在线价格沿用 {time} 的那份（最近一次更新失败）",
  "usage.priceOnlineFailed": "在线价格暂未获取，仅用内置表",
  "usage.priceOnlinePending": "在线价格获取中",
  "usage.priceHint":
    "金额先按内置单价表算；内置表没收录、或只认得到更粗的名字（如日期快照、新子型号）时，用每天从 LiteLLM 公开价格表（经 jsDelivr）更新的在线价补上。同名时以内置表为准。只下载公开的价格文件，不上传任何数据。",
  "usage.priceErrorHint": "最近一次更新失败：{error}",
  // ---- 还没捕获过 usage 的行（并集之后新加的 Key 会立刻以这个形态出现）----
  //
  // 🔴 绝不显示 $0：那读起来是「已经在服务、而且免费」。也不猜成因 —— 三种可能
  // （尚未使用 / 流仍在进行 / 上游不回 usage）我们分辨不出，故写成条件句。
  "usage.noUsageYet": "尚无用量",
  "usage.noUsageYetHint":
    "还没有收到上游回报的用量。可能是这条 Key 尚未被使用，或流式回答仍在进行（用量在流结束时才补记），也可能这个中转站不返回 usage 字段。",
  // ---- 每 Key 花费预算（累计口径）----
  // 判据在后端（usage_cost::budget_status 与 over_budget 那一位），前端只渲染，不自己比大小。
  "budget.label": "花费预算（美元，选填）",
  "budget.hint": "累计估算花费达到它时：用量页标红提醒，路由也会优先用别的 Key（仍会在其它 Key 都不可用时兜底，不会因此让请求失败）。留空 = 不限。只是估算，与实际账单可能有差。",
  "budget.invalid": "请填一个大于 0 的数字（美元），或留空表示不限。",
  "usage.overBudget": "超预算",
  "usage.overBudgetBanner": "有 {n} 条 Key 的累计估算花费已达/超过你设的预算：{keys}",
};

export const usageEn: Dict = {
  "usage.title": "Usage",
  "usage.subtitle": "Token consumption and estimated spend by category and key (cumulative across restarts)",
  "usage.refresh": "Refresh",
  "usage.refreshing": "Refreshing…",
  "usage.loading": "Loading…",
  "usage.empty": "No keys configured and no usage history yet",
  "usage.input": "Input",
  "usage.output": "Output",
  "usage.cacheRead": "Cache read",
  "usage.total": "Total",
  "usage.colCategory": "Category",
  "usage.colKey": "Key",
  "usage.systemLevel": "(system)",
  "usage.since": "Counting since",
  "usage.sinceHint": "Saved every minute (only when there is new usage); an unexpected exit loses at most 1 minute",
  // Cost & trend. The first three cards are calendar periods (week starts Monday, month on the 1st,
  // local time zone); the main number is spend, with tokens as a sub-line.
  "usage.today": "Today",
  "usage.thisWeek": "This week",
  "usage.thisMonth": "This month",
  "usage.periodTokens": "{tokens} tokens",
  "usage.periodHint": "Calendar periods in your local time zone (weeks start on Monday, months on the 1st). Amounts are estimates, not a bill.",
  "usage.periodUnpriced": "{tokens} more tokens couldn't be priced and aren't included",
  "usage.estCost": "Est. spend",
  "usage.trend7d": "Usage, last 7 days",
  "usage.trendTip": "{date} · {tokens} tokens · ~{cost}",
  "usage.trendTipPartial": "{date} · {tokens} tokens · ~{cost} ({unpriced} tokens couldn't be priced)",
  "usage.colCost": "Cost",
  "usage.estimateHint":
    "Estimated from list prices (the built-in table plus a public price list updated daily) × your cost multiplier — not a bill. Each model is priced at its own rate; providers may price differently, so adjust the multiplier in the key editor to calibrate.",
  "usage.costExactHint": "From the built-in price table, multiplier {multiplier}",
  "usage.costFamilyHint":
    "Part of this is an estimate (marked ≈): usage recorded before the upgrade has no model and is priced as this key's representative model, or a model isn't in the table and is priced by its family name. Multiplier {multiplier}; may deviate significantly.",
  "usage.breakdownTitle": "By model:",
  "usage.breakdownLegacy": "before the upgrade (no model recorded, priced as {model})",
  "usage.breakdownLegacyUnknown": "before the upgrade (no model recorded)",
  "usage.breakdownUnpriced": "can't be priced",
  "usage.partialMark": "+?",
  "usage.partialHint": "Part of this row's usage can't be priced; the amount only covers the part that can.",
  "usage.costUnknownHint":
    "Can't estimate: no recognizable model name for this key. Set a default fallback model or fetch the model list to enable estimation.",

  // ---- The four reasons a row can't be priced ----
  // The old build collapsed all four into one "—" with a single catch-all hint that was
  // only true for the fourth. For the other three it sent the user to do something
  // that changes nothing — and afterwards the screen shows no new information.
  "usage.reason.aggregate":
    "This is brain-aggregation usage. Older versions didn't attribute it to a specific key, so there's no price or multiplier to apply. New aggregation usage is attributed to each participating key; this row is historical — it stops growing but is never dropped.",
  "usage.reason.keyDeleted":
    "This row's key is no longer in your config and its representative model and cost multiplier weren't kept, so the usage recorded before the upgrade (no model) can't be priced. Its tokens still count toward the totals; use Clean up to remove the row if you don't need it.",
  "usage.keyDeletedTag": "(deleted)",
  "usage.reason.noModelName":
    "Usage recorded before the upgrade has no model, and this key has no model name to price it with. Set a default fallback model in the key editor, or fetch the model list once.",
  "usage.reason.modelNotInTable":
    "Model \"{model}\" isn't in the price tables ({tables}), so that usage isn't included in the amount. It may be a new model or a provider-specific name.",
  "usage.reason.tablesBoth": "built-in table verified {date}; the online list of {n} models doesn't have it either",
  "usage.reason.tablesBuiltinOnly": "built-in table verified {date}; the online list hasn't been fetched yet",
  "usage.unpricedBanner": "{n} row(s) can't be (fully) priced:",
  "usage.unpricedGroup.aggregate": "{n} from legacy brain aggregation (no key attribution)",
  "usage.unpricedGroup.keyDeleted": "{n} whose key is no longer configured and has no saved pricing — use Clean up in the table if you don't need them",
  "usage.unpricedGroup.noModelName": "{n} with no representative model — set a default fallback model",
  "usage.unpricedGroup.modelNotInTable": "{n} whose model isn't in the price table: {models}",
  "usage.estCostExcluded": "+{n} row(s) not (fully) included",
  "usage.pricedBy": "mainly priced as {model}",
  "usage.purge": "Clean up",
  "usage.purgeHint": "Delete all usage records of this key, which is no longer configured (can't be undone)",
  "usage.purgeConfirm": "Confirm",
  "usage.purgeNotPersisted": "Removed from the list, but the usage file couldn't be written this time (read-only or locked); it may reappear after a restart.",
  "usage.deleteKeyNote": "This key's usage records will be deleted too. This can't be undone.",
  "usage.priceBuiltin": "Built-in price table verified {date}",
  "usage.priceOnline": "online prices for {n} models (updated {time})",
  "usage.priceOnlineStale": "using online prices from {time} (the latest update failed)",
  "usage.priceOnlineFailed": "online prices not available yet; using the built-in table only",
  "usage.priceOnlinePending": "fetching online prices",
  "usage.priceHint":
    "Amounts use the built-in price table first. When a model isn't in it, or only a coarser name matches (dated snapshots, new sub-models), prices from the LiteLLM public price list (via jsDelivr, updated daily) fill the gap. The built-in table wins when both have the same name. Only the public price file is downloaded; nothing is uploaded.",
  "usage.priceErrorHint": "Latest update failed: {error}",
  "usage.noUsageYet": "No usage yet",
  "usage.noUsageYetHint":
    "No usage reported by the upstream yet. This key may not have been used, a streaming reply may still be in flight (usage is recorded when the stream ends), or this provider may not return a usage field.",
  // ---- Per-key spend budget (cumulative) ----
  "budget.label": "Budget (USD, optional)",
  "budget.hint": "When estimated cumulative spend reaches it, the usage page flags this key and routing prefers other keys (still used as a fallback if no other key is available, so requests won't fail because of it). Empty = no limit. Estimates only; may differ from your actual bill.",
  "budget.invalid": "Enter a positive number (USD), or leave empty for no limit.",
  "usage.overBudget": "Over budget",
  "usage.overBudgetBanner": "{n} key(s) reached or exceeded the budget you set: {keys}",
};
