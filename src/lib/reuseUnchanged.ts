/**
 * 轮询拿回新数组时，**逐条复用内容未变的旧对象**，保住引用相等。
 *
 * 为什么需要：`listKeys` / `listAllEvents` 每次都返回一份全新反序列化的对象
 * （IPC 过来的必然是新对象），于是数组里每个元素的引用每次轮询都变一次 —— 即使内容
 * 一模一样。这让 `React.memo` 完全失效：默认浅比较看的是 `prevProps.k === nextProps.k`，
 * 引用变了就判定「要重渲染」。实测确认过：轮询后 `k` 引用必变、`JSON.stringify` 完全相同。
 *
 * 所以性能优化的正确落点在**数据层而非组件层**：在这里把「内容没变」翻译成「引用不变」，
 * 组件侧的 memo 才有意义。反过来只在组件上包 memo 是白做的（方案文档原本就写错了这一点）。
 *
 * 判据用 `JSON.stringify` 而非逐字段比：ProviderKey / EventLogEntry 字段多且会随需求增删
 * （逐字段比较器在新增字段时会漏掉、悄悄退化成「永远不相等」），字符串化虽略慢但不漏字段。
 * 数量级是几十条 Key、最多 500 条事件，每 2~5s 一次，开销可忽略。
 *
 * 注意 `JSON.stringify` 对**键顺序敏感**：同样的内容若键序不同会被判为「变了」。
 * 这里可接受 —— 数据都来自同一个后端序列化路径，键序稳定；退化后果也只是多一次重渲染，
 * 不会出错。
 */
export function reuseUnchanged<T extends { id: string }>(prev: T[], next: T[]): T[] {
  const byId = new Map(prev.map((p) => [p.id, p]));
  let allSame = prev.length === next.length;
  const merged = next.map((n, i) => {
    const old = byId.get(n.id);
    if (old && JSON.stringify(old) === JSON.stringify(n)) {
      if (prev[i] !== old) allSame = false; // 内容同但位置变了（排序变更）
      return old; // 复用旧对象 → 引用不变 → memo 生效
    }
    allSame = false;
    return n;
  });
  // 整个列表都没变时连数组引用也保住，避免订阅了该数组的组件白跑一次渲染。
  return allSame ? prev : merged;
}
