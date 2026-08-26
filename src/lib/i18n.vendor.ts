// 厂商管理页的本地化词条。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上、余量为 0）。粒度按**页面/组件**分，
// 与 i18n.usage.ts / i18n.brandPicker.ts 同一口径。
//
// ⚠️ 新增分片必须同时加进 src/lib/i18n.test.ts 的 SOURCES 与 CHUNKS 两张表 ——
// 只加一处的话，「拆了文件却没接进主词典」或「zh/en 不对称」会静默逃过检查。

type Dict = Record<string, string>;
export const vendorZh: Dict = {
  "vendor.title": "厂商管理",
  "vendor.desc": "维护厂商预设。选中厂商后会自动填入其 Base URL 与协议（仍可手动修改）。",
  "vendor.add": "新增厂商",
  "vendor.builtin": "内置",
  "vendor.builtinLocked": "内置厂商不可编辑或删除",
  "vendor.name": "显示名",
  "vendor.namePlaceholder": "如：我的中转",
  "vendor.baseUrl": "默认 Base URL",
  "vendor.protocol": "默认协议",
  "vendor.icon": "图标",
  "vendor.iconPick": "选择图标",
  "vendor.iconClear": "清除",
  "vendor.iconPresetLabel": "或直接选一个主流厂商图标（再点一次取消，回到自动识别）",
  "vendor.iconHint": "可选。支持 PNG/JPG/SVG/WebP，≤256KB；不设则按厂商名/地址自动识别，识别不出用首字母占位。",
  "vendor.iconTooLarge": "图标文件过大（上限 {max}），请换更小的图",
  "vendor.iconReadFailed": "读取图标文件失败",
  "vendor.edit": "编辑",
  "vendor.delete": "删除",
  "vendor.save": "保存",
  "vendor.cancel": "取消",
  "vendor.deleteConfirm": "确定删除厂商「{name}」？已使用它的 Key 不受影响。",
  "vendor.empty": "还没有自定义厂商",
  "vendor.errNeedName": "请填写显示名",
  "vendor.errNeedUrl": "请填写 Base URL",
};

export const vendorEn: Dict = {
  "vendor.title": "Vendors",
  "vendor.desc": "Manage vendor presets. Selecting a vendor auto-fills its Base URL and protocol (still editable).",
  "vendor.add": "Add vendor",
  "vendor.builtin": "Built-in",
  "vendor.builtinLocked": "Built-in vendors can't be edited or deleted",
  "vendor.name": "Display name",
  "vendor.namePlaceholder": "e.g. My relay",
  "vendor.baseUrl": "Default Base URL",
  "vendor.protocol": "Default protocol",
  "vendor.icon": "Icon",
  "vendor.iconPick": "Choose icon",
  "vendor.iconClear": "Clear",
  "vendor.iconPresetLabel": "Or pick a known provider icon (click again to clear and auto-detect)",
  "vendor.iconHint": "Optional. PNG/JPG/SVG/WebP, ≤256KB. Otherwise auto-detected from name/URL, falling back to an initial.",
  "vendor.iconTooLarge": "Icon file too large (max {max}), pick a smaller one",
  "vendor.iconReadFailed": "Failed to read icon file",
  "vendor.edit": "Edit",
  "vendor.delete": "Delete",
  "vendor.save": "Save",
  "vendor.cancel": "Cancel",
  "vendor.deleteConfirm": "Delete vendor \"{name}\"? Keys already using it are unaffected.",
  "vendor.empty": "No custom vendors yet",
  "vendor.errNeedName": "Display name is required",
  "vendor.errNeedUrl": "Base URL is required",
};
