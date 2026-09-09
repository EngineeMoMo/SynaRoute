// Key 顺序（故障转移优先级）相关的本地化词条：上移/下移按钮 + 鼠标拖放。
//
// 从 i18n.ts 拆出来的：那个文件冻结在棘轮上、余量为 0，而本轮要给拖放补文案。
// 顺带把已有的 `key.moveUp` / `key.moveDown` 一起搬过来 —— 同一件事的词条散在两个文件里，
// 改文案时必然只改一处。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致，由 `i18n.test.ts` 机械校验（新分片已加进 SOURCES）。
//
// 🔴 **文案只承诺「故障转移的默认顺序」，不承诺「每个请求一定先打第一条」**：
// 真实候选排序是 `(模型服务把握, 余额是否耗尽, priority)`，还要过熔断 / 模型锁 /
// 配额窗口三道过滤（见 `model_pool::rank_candidates`）。写成「一定先用第一条」在
// 「第一条正在熔断」时就是假话，而用户会据此排除掉真正的原因。

type Dict = Record<string, string>;

export const orderingZh: Dict = {
  "key.moveUp": "上移（提高优先级，故障转移更早使用）",
  "key.moveDown": "下移（降低优先级）",
  // 把手的 tooltip 要同时说清「怎么用」与「键盘怎么办」：拖放在辅助技术下不可达，
  // 而上下移按钮就在旁边，一句话就能把键盘用户指过去。
  "key.dragHandle": "拖动调整顺序",
  "key.dragHandleHint": "按住拖动即可调整顺序；也可用旁边的上移 / 下移按钮（支持键盘）",
};

export const orderingEn: Dict = {
  "key.moveUp": "Move up (higher priority, tried earlier on failover)",
  "key.moveDown": "Move down (lower priority)",
  "key.dragHandle": "Drag to reorder",
  "key.dragHandleHint":
    "Hold and drag to reorder; the move up / down buttons next to it work too (keyboard-friendly)",
};
