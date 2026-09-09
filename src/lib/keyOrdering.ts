// Key 拖放排序的**纯计算**部分：指针位置 → 插入锚点，以及乐观重排。
//
// 抽成纯函数而不是写在组件里，理由是本仓没有 jsdom —— 组件里的逻辑测不到，
// 而这两个函数恰恰是最容易写错、且错法**静默**的地方（偏一格、把自己当锚点、
// 往下拖时插错位置，全都不报错，只是「拖到的位置和松手后的位置不一样」）。
//
// 同一套语义在三处必须一致：本文件、`mockData.reorderKey`、后端
// `key_order::reorder_before`。锚点语义统一为「插到 `beforeId` **之前**，
// `undefined` = 末尾」。

/** 一张卡片在视口里的纵向位置（只取拖放需要的两项）。 */
export interface CardBox {
  id: string;
  /** 卡片顶边（相对视口，`getBoundingClientRect().top`） */
  top: number;
  /** 卡片底边 */
  bottom: number;
}

/**
 * 指针停在 `pointerY` 时，被拖动的 `draggingId` 应该插到谁之前。
 *
 * 返回 `undefined` 表示「插到末尾」。
 *
 * 判据是**卡片中线**：指针在某张卡的上半部分 → 插到它之前；下半部分 → 插到它之后
 * （也就是下一张之前）。这与用户看到的插入线位置一一对应。
 *
 * 🔴 **必须先把 source 自己排除掉**。它仍在列表里占着位置，不排除会得到两种错法：
 * 指针停在自己身上时算出「插到自己之前」（后端会把它当 no-op，于是拖动看起来毫无反应），
 * 以及往下拖一格时锚点算成自己的后继 —— 那同样是原地不动（见后端那条测试的说明）。
 *
 * `boxes` 要求按视觉顺序（自上而下）传入，与列表渲染顺序一致。
 */
export function dropAnchorAt(
  boxes: CardBox[],
  draggingId: string,
  pointerY: number,
): string | undefined {
  const others = boxes.filter((b) => b.id !== draggingId);
  for (const b of others) {
    // 中线之上 → 落在这张卡前面。
    if (pointerY < (b.top + b.bottom) / 2) return b.id;
  }
  // 比所有卡片的中线都低 → 末尾。
  return undefined;
}

/**
 * 乐观重排：把 `keyId` 移到 `beforeId` 之前（`undefined` = 末尾），返回新的 id 顺序。
 *
 * 只算顺序、不碰 `priority` —— 调用方按下标重编号。语义与后端逐字对应：
 * **先摘掉 source，再定位锚点**。反过来（先定位）会在「往下拖」时偏一格。
 *
 * 找不到 source 时原样返回：那说明列表已经被别的来源改过（切分类、磁盘自愈重载），
 * 此时凭一份陈旧快照猜一个位置比什么都不做糟。
 */
export function reorderIds(
  ids: string[],
  keyId: string,
  beforeId: string | undefined,
): string[] {
  const from = ids.indexOf(keyId);
  if (from < 0 || beforeId === keyId) return ids;
  const next = [...ids];
  next.splice(from, 1);
  const at = beforeId ? next.indexOf(beforeId) : -1;
  if (at < 0) next.push(keyId);
  else next.splice(at, 0, keyId);
  return next;
}
