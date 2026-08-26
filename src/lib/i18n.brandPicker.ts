// 品牌挑选器的本地化词条。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上、余量为 0）。粒度按**组件**分，
// 与 i18n.usage.ts 同一口径：改一处 UI 的文案时要同时看 zh/en 两份，同组放一起最省事。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致 —— 由 src/lib/i18n.test.ts 机械校验
// （它遍历 SOURCES 里的每个分片；**新增分片必须加进那张表**，
// 否则那一整页词条会静默脱离保护范围）。

type Dict = Record<string, string>;

export const brandPickerZh: Dict = {
  "brandPicker.search": "搜索厂商（名称、GLM、bigmodel…）",
  // 分组名。32 个品牌平铺成一排是找不到东西的，故按「这是哪一类供应商」分四段。
  "brandPicker.group.official": "国际原厂",
  "brandPicker.group.china": "国内原厂",
  "brandPicker.group.gateway": "聚合 / 中转 / 托管",
  "brandPicker.group.local": "本机推理",
  "brandPicker.noMatch": "没有匹配「{q}」的厂商",
  "common.clear": "清除",
};

export const brandPickerEn: Dict = {
  "brandPicker.search": "Search vendors (name, GLM, bigmodel…)",
  "brandPicker.group.official": "Global providers",
  "brandPicker.group.china": "China providers",
  "brandPicker.group.gateway": "Gateways & hosting",
  "brandPicker.group.local": "Local inference",
  "brandPicker.noMatch": "No vendor matches “{q}”",
  "common.clear": "Clear",
};
