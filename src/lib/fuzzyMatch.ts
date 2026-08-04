/**
 * 命令面板用的轻量模糊匹配（UX#7）。
 *
 * **为什么自己写而不装 fuse.js / cmdk**：本项目是桌面应用、包体积敏感，且整套 UI 组件
 * 都是自建的。这里要的功能就是「子序列匹配 + 一个够用的排序分」，几十行能写完，
 * 引一个库要多背几十 KB 和一套用不上的配置面。
 *
 * 匹配语义是**子序列**（subsequence）而非子串：查询 "gcli" 能命中 "glm-claude-cli"。
 * 这对本场景很关键 —— 用户记得的是「那条 glm 的 Key」而不是它的完整备注名。
 */

/** 匹配结果：命中的字符下标（供高亮）与排序分（越大越靠前）。 */
export interface FuzzyResult {
  matched: boolean;
  score: number;
  /** 命中字符在 target 里的下标，已升序 */
  indices: number[];
}

const NO_MATCH: FuzzyResult = { matched: false, score: 0, indices: [] };

/**
 * 把 query 当作子序列在 target 里找。大小写不敏感。
 *
 * 计分规则（都是为了让「更像用户心里想的那条」排前面）：
 * - 连续命中加分：`claude` 匹配 "claude-cli" 应该远胜于零散命中同样多字符的串。
 * - 命中在词首（开头、或紧跟 `-` `_` `.` `/` 空格）额外加分：缩写式查询 "ccli" 找
 *   "claude-cli" 时，命中的都是词首，应该排在偶然含这些字母的长串前面。
 * - 目标越短分越高：同样命中程度下，短的更可能是用户要找的那条。
 *
 * 空 query 视为**全部命中**（分 0），这样面板刚打开、还没输入时能列出全部候选。
 */
export function fuzzyMatch(query: string, target: string): FuzzyResult {
  const q = query.trim().toLowerCase();
  if (!q) return { matched: true, score: 0, indices: [] };
  const t = target.toLowerCase();
  if (!t) return NO_MATCH;

  const indices: number[] = [];
  let score = 0;
  let ti = 0;
  let lastHit = -2; // 用 -2 保证首个命中不会被误判成「与上一个连续」

  for (const ch of q) {
    let found = -1;
    while (ti < t.length) {
      if (t[ti] === ch) {
        found = ti;
        break;
      }
      ti++;
    }
    if (found < 0) return NO_MATCH; // 有一个字符找不到 → 整体不匹配
    indices.push(found);
    score += 1;
    if (found === lastHit + 1) score += 3; // 连续命中
    const prev = found > 0 ? t[found - 1] : "";
    if (found === 0 || prev === "-" || prev === "_" || prev === "." || prev === "/" || prev === " ") {
      score += 2; // 词首命中
    }
    lastHit = found;
    ti = found + 1;
  }

  // 目标越短越优先。用减法而不是除法：除法在极短串上会把分数放大到压过连续命中。
  score += Math.max(0, 20 - t.length) / 10;
  return { matched: true, score, indices };
}

/**
 * 对多个字段取最佳匹配。
 *
 * 用途：一条 Key 有备注名、厂商、若干模型名，用户可能按其中任何一个来找。
 * 返回分最高的那个字段的结果，并带上是哪个字段命中的（供界面显示「匹配到：模型名 xxx」）。
 */
export function fuzzyMatchAny(
  query: string,
  fields: { label: string; value: string }[],
): (FuzzyResult & { field: string; value: string }) | null {
  let best: (FuzzyResult & { field: string; value: string }) | null = null;
  for (const f of fields) {
    if (!f.value) continue;
    const r = fuzzyMatch(query, f.value);
    if (!r.matched) continue;
    if (!best || r.score > best.score) best = { ...r, field: f.label, value: f.value };
  }
  return best;
}
