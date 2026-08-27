// Rust 源码的「生产段 / 测试段」切分判据。**单一事实来源**。
//
// 🔴 为什么单独一个文件：这套判据同时被 `check-ratchet.mjs`（数行数）与
// `check-forbidden.mjs`（决定扫哪些行）使用。原先两边各写了一份，且已经**漂移**：
// ratchet 那份认 `pub(crate) mod tests`，forbidden 那份只认裸 `mod tests`。
//
// 更麻烦的是，直接让一个脚本 `import` 另一个脚本会**触发它的顶层检查逻辑**
// （check-ratchet.mjs 一被导入就跑完整棘轮并打印结果，于是策略门的输出里混进棘轮的输出、
// 检查还跑了两遍）。抽成无副作用的纯模块是唯一干净的解法。
//
// 加新的消费者时从这里导入，不要再抄第三份。

/**
 * 尾部 `#[cfg(test)] mod tests {` 的起始行号（1-based）；没有则返回 null。
 *
 * ⚠️ **可见性修饰符必须一起认**（`pub(crate) mod tests`）。跨模块复用测试夹具时会用到它
 * （diagnostics.rs 就借 service.rs 的 `temp_store` / `key`，避免夹具抄两份后各自漂移）。
 * 第一版只认裸 `mod tests`，于是给测试模块加一个 `pub(crate)` 就让这里返回 null、
 * 整份文件被当成生产段 —— service.rs 一下从 1608 报到 2478。
 *
 * 那次的失效方向恰好是**响亮的**（多算行数 → 门变红），所以被立刻发现；
 * 但同一个盲区在 `check-forbidden.mjs` 里的方向是**反的、也更隐蔽**：测试段被当成生产段去扫，
 * 于是测试夹具里的假路径会被 no-hardcoded-local-paths 报成违规。
 * 故这里按「可选修饰符」写全，别再收窄。
 */
export function testModStartLine(src) {
  const lines = src.split("\n");
  const MOD_TESTS = /^\s{0,4}(pub(\([^)]*\))?\s+)?mod tests\b/;
  for (let i = lines.length - 1; i >= 0; i--) {
    if (MOD_TESTS.test(lines[i])) {
      // 往上找紧邻的 `#[cfg(test)]`（允许中间夹注释行）
      for (let j = i - 1; j >= 0 && j >= i - 8; j--) {
        const t = lines[j].trim();
        if (t === "" || t.startsWith("//")) continue;
        if (/^#\[cfg\(test\)\]$/.test(t)) return j + 1;
        break;
      }
    }
  }
  return null;
}

/** 一个文件的「生产段」行数。 */
export function prodLines(src) {
  const cut = testModStartLine(src);
  const lines = src.split("\n");
  return cut === null ? lines.length : cut - 1;
}
