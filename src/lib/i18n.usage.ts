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
  "usage.loading": "加载中…",
  "usage.empty": "暂无用量记录（有转发流量后这里会出现数据）",
  "usage.input": "输入",
  "usage.output": "输出",
  "usage.cacheRead": "缓存读取",
  "usage.total": "合计",
  "usage.colCategory": "分类",
  "usage.colKey": "Key",
  "usage.systemLevel": "（系统级）",
  "usage.since": "统计起点",
  "usage.sinceHint": "每分钟自动保存一次（仅在有新消耗时写盘），意外退出最多丢 1 分钟",
  // 第⑤批：金额与趋势
  //
  // 前三格显示的是 **token 数**（不是钱），故标签带单位后缀 —— 四格并排且第四格是金额，
  // 不写单位的话「今日 1.2M」很容易被读成 120 万美元。
  "usage.today": "今日 token",
  "usage.thisWeek": "近 7 日 token",
  "usage.thisMonth": "近 30 日 token",
  "usage.estCost": "累计花费（估算）",
  "usage.trend7d": "近 7 日 token 消耗",
  "usage.colCost": "花费",
  "usage.estimateHint":
    "按内置官方单价 × 你填的计费倍率估算，不是账单。中转站实际计价可能不同；可在 Key 编辑器里调整「计费倍率」校准。",
  "usage.costExactHint": "按官方单价表计算，倍率 {multiplier}",
  "usage.costFamilyHint":
    "该模型不在内置单价表里，按模型家族名（opus / sonnet / glm 等）估算，倍率 {multiplier}。偏差可能较大。",
  "usage.costUnknownHint":
    "无法估算：这条 Key 没有可识别的模型名。在 Key 里设置「默认兜底模型」或拉取模型列表后即可估算。",
  "usage.unpricedHint": "有 {n} 条 Key 无法估算花费（模型名不在单价表中，金额列显示「—」）。",

  // ---- 「为什么算不出花费」的四种成因 ----
  //
  // 旧实现把四种成因压成同一个「—」，只给一句放之四海的提示：「这条 Key 没有可识别的
  // 模型名。在 Key 里设置默认兜底模型…」。它**只对第四种成立**，对另外三种都是把用户
  // 送去做无效操作 —— 而做完之后界面上没有任何新信息，用户只能认为功能坏了。
  "usage.reason.aggregate":
    "这笔是「大脑聚合」的消耗，旧版本没把它记到具体 Key 上，因此取不到单价与倍率。升级后新产生的聚合用量会归到各参与者 Key 上；这一行是历史数据，会停止增长但不会消失。",
  "usage.reason.keyDeleted":
    "这一行的 Key 是在「删除前留档」功能上线之前删掉的，它的代表模型与计费倍率当时没有留下来，所以金额算不出。token 数仍然计入总量。之后删除的 Key 不会再有这个问题 —— 金额会照旧显示。",
  "usage.keyDeletedTag": "（已删除）",
  "usage.reason.noModelName":
    "这条 Key 没有可识别的模型名。在 Key 编辑器里设置「默认兜底模型」或拉取一次模型列表即可估算。",
  "usage.reason.modelNotInTable":
    "模型「{model}」不在内置单价表里（表核对日期 {date}）。可在 Key 编辑器的「计费」里填计费倍率来校准金额。",
  // 横幅按成因分组，每组一句、各自指路。
  "usage.unpricedBanner": "有 {n} 行无法估算花费（金额列显示「—」）：",
  "usage.unpricedGroup.aggregate": "{n} 行是旧版大脑聚合用量（无 Key 归属）",
  "usage.unpricedGroup.keyDeleted": "{n} 行的 Key 在「删除前留档」上线前就删掉了（不可恢复）",
  "usage.unpricedGroup.noModelName": "{n} 行没有代表模型名 —— 去 Key 里设「默认兜底模型」即可",
  "usage.unpricedGroup.modelNotInTable": "{n} 行的模型不在单价表：{models}",
  // 累计金额那一格：**必须**标出「有几行没算进去」。
  // 只在下方横幅里说是不够的 —— 那个数字自己标着「累计花费」，用户会当成总计。
  "usage.estCostExcluded": "＋{n} 行未计入",
  "usage.pricedBy": "按 {model} 估算",
  "usage.tableDate": "单价表核对于 {date}",
};

export const usageEn: Dict = {
  "usage.title": "Usage",
  "usage.subtitle": "Token consumption and estimated spend by category and key (cumulative across restarts)",
  "usage.refresh": "Refresh",
  "usage.loading": "Loading…",
  "usage.empty": "No usage yet (data appears after forwarding traffic)",
  "usage.input": "Input",
  "usage.output": "Output",
  "usage.cacheRead": "Cache read",
  "usage.total": "Total",
  "usage.colCategory": "Category",
  "usage.colKey": "Key",
  "usage.systemLevel": "(system)",
  "usage.since": "Counting since",
  "usage.sinceHint": "Saved every minute (only when there is new usage); an unexpected exit loses at most 1 minute",
  // Batch ⑤: cost & trend
  // First three cards show **token counts**, not money — the unit suffix matters
  // because the fourth card in the same row is a dollar amount.
  "usage.today": "Today (tokens)",
  "usage.thisWeek": "Last 7d (tokens)",
  "usage.thisMonth": "Last 30d (tokens)",
  "usage.estCost": "Est. spend",
  "usage.trend7d": "Token usage, last 7 days",
  "usage.colCost": "Cost",
  "usage.estimateHint":
    "Estimated from built-in list prices × your cost multiplier — not a bill. Providers may price differently; adjust the multiplier in the key editor to calibrate.",
  "usage.costExactHint": "From the built-in price table, multiplier {multiplier}",
  "usage.costFamilyHint":
    "This model isn't in the built-in price table; estimated from its family name (opus / sonnet / glm …), multiplier {multiplier}. May deviate significantly.",
  "usage.costUnknownHint":
    "Can't estimate: no recognizable model name for this key. Set a default fallback model or fetch the model list to enable estimation.",
  "usage.unpricedHint": "{n} key(s) can't be priced (model not in the price table); their cost shows as \"—\".",

  // ---- The four reasons a row can't be priced ----
  // The old build collapsed all four into one "—" with a single catch-all hint that was
  // only true for the fourth. For the other three it sent the user to do something
  // that changes nothing — and afterwards the screen shows no new information.
  "usage.reason.aggregate":
    "This is brain-aggregation usage. Older versions didn't attribute it to a specific key, so there's no price or multiplier to apply. New aggregation usage is attributed to each participating key; this row is historical — it stops growing but is never dropped.",
  "usage.reason.keyDeleted":
    "This row's key was deleted before the \"snapshot before delete\" feature shipped, so its representative model and cost multiplier were never recorded and the amount can't be computed. Its tokens still count toward the totals. Keys deleted from now on keep their pricing.",
  "usage.keyDeletedTag": "(deleted)",
  "usage.reason.noModelName":
    "No recognizable model name for this key. Set a default fallback model in the key editor, or fetch the model list once.",
  "usage.reason.modelNotInTable":
    "Model \"{model}\" isn't in the built-in price table (table verified {date}). You can calibrate the amount with a cost multiplier under Billing in the key editor.",
  "usage.unpricedBanner": "{n} row(s) can't be priced (cost shows as \"—\"):",
  "usage.unpricedGroup.aggregate": "{n} from legacy brain aggregation (no key attribution)",
  "usage.unpricedGroup.keyDeleted": "{n} whose key was deleted before pricing was snapshotted (unrecoverable)",
  "usage.unpricedGroup.noModelName": "{n} with no representative model — set a default fallback model",
  "usage.unpricedGroup.modelNotInTable": "{n} whose model isn't in the price table: {models}",
  "usage.estCostExcluded": "+{n} row(s) excluded",
  "usage.pricedBy": "priced as {model}",
  "usage.tableDate": "Price table verified {date}",
};
