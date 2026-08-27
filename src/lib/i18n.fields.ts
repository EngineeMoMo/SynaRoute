// 各处**独立字段组件**的本地化词条（`CustomHeadersField` / `LanTokenPanel` …）。
//
// 命名按「一组独立字段」而不是某一个组件：这些组件都是为了绕开 KeyEditor /
// SettingsPage 的棘轮而抽出来的，每个只有几条文案，各占一个 sidecar 会让
// i18n.ts 的 import 与 spread 迅速膨胀（而那边余量恒为 0）。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上）。粒度按**组件**分，
// 与 i18n.usage.ts / i18n.brandPicker.ts 同一口径。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致 —— 由 src/lib/i18n.test.ts 机械校验
// （它遍历 SOURCES 里的每个分片；**新增分片必须加进那张表**，
// 否则那一整页词条会静默脱离保护范围）。

type Dict = Record<string, string>;

export const fieldsZh: Dict = {
  "customHeaders.label": "自定义请求头",
  // 说明里必须点出「哪些填不了」：用户最可能想填的就是 authorization（想换个鉴权方式），
  // 而那正是被拒的一条。不预先说清，他会先填一遍、被拒、再来猜原因。
  "customHeaders.desc":
    "转发到本 Key 时额外带上的请求头，JSON 对象。常见用途：OpenRouter 的 HTTP-Referer / X-Title，或中转站要求的自有标识头。鉴权、内容长度、压缩协商这几类由 SynaRoute 自己管理，填了会被拒。",
  "customHeaders.placeholder": '{"HTTP-Referer": "https://your.app", "X-Title": "YourApp"}',
  "customHeaders.invalid": "格式有问题：{err}",
  "customHeaders.reserved": "这些头由 SynaRoute 管理，不能自定义：{names}",
  "customHeaders.ok": "{n} 个自定义头",
  "customHeaders.empty": "未设置",
  "lanToken.label": "局域网接入令牌",
  "lanToken.none": "尚未生成（启动代理时会自动生成）",
  "lanToken.show": "显示", "lanToken.hide": "隐藏", "lanToken.copy": "复制",
  "lanToken.copied": "已复制到剪贴板", "lanToken.copyFailed": "复制失败，请手动选中",
  "lanToken.regenerate": "重新生成",
  "lanToken.regenerated": "已重新生成，请更新所有局域网客户端",
  "lanToken.confirmDesc": "重新生成后旧令牌立即失效，每一个已配好的局域网客户端都要改成新令牌才能继续用。本机客户端不受影响。",
  "lanToken.confirmYes": "确认重新生成",
  "lanToken.hint": "把它填进局域网客户端的 API Key（或 Authorization: Bearer）。本机客户端不需要填。",
};

export const fieldsEn: Dict = {
  "customHeaders.label": "Custom request headers",
  "customHeaders.desc":
    "Extra headers sent upstream for this key, as a JSON object. Typical uses: OpenRouter's HTTP-Referer / X-Title, or a relay's own identifying header. Auth, content-length and compression negotiation are managed by SynaRoute and will be rejected.",
  "customHeaders.placeholder": '{"HTTP-Referer": "https://your.app", "X-Title": "YourApp"}',
  "customHeaders.invalid": "Invalid: {err}",
  "customHeaders.reserved": "These headers are managed by SynaRoute and cannot be overridden: {names}",
  "customHeaders.ok": "{n} custom header(s)",
  "customHeaders.empty": "Not set",
  "lanToken.label": "LAN access token",
  "lanToken.none": "Not generated yet (created when the proxy starts)",
  "lanToken.show": "Show", "lanToken.hide": "Hide", "lanToken.copy": "Copy",
  "lanToken.copied": "Copied to clipboard", "lanToken.copyFailed": "Copy failed — select it manually",
  "lanToken.regenerate": "Regenerate",
  "lanToken.regenerated": "Regenerated — update every LAN client",
  "lanToken.confirmDesc": "Regenerating invalidates the old token immediately. Every already-configured LAN client must be updated to the new one. Local clients are unaffected.",
  "lanToken.confirmYes": "Yes, regenerate",
  "lanToken.hint": "Put it in the LAN client's API key field (or Authorization: Bearer). Local clients do not need it.",
};
