// 「自定义请求头」字段的本地化词条（B1，docs/14 §21.1）。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上）。粒度按**组件**分，
// 与 i18n.usage.ts / i18n.brandPicker.ts 同一口径。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致 —— 由 src/lib/i18n.test.ts 机械校验
// （它遍历 SOURCES 里的每个分片；**新增分片必须加进那张表**，
// 否则那一整页词条会静默脱离保护范围）。

type Dict = Record<string, string>;

export const customHeadersZh: Dict = {
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
};

export const customHeadersEn: Dict = {
  "customHeaders.label": "Custom request headers",
  "customHeaders.desc":
    "Extra headers sent upstream for this key, as a JSON object. Typical uses: OpenRouter's HTTP-Referer / X-Title, or a relay's own identifying header. Auth, content-length and compression negotiation are managed by SynaRoute and will be rejected.",
  "customHeaders.placeholder": '{"HTTP-Referer": "https://your.app", "X-Title": "YourApp"}',
  "customHeaders.invalid": "Invalid: {err}",
  "customHeaders.reserved": "These headers are managed by SynaRoute and cannot be overridden: {names}",
  "customHeaders.ok": "{n} custom header(s)",
  "customHeaders.empty": "Not set",
};
