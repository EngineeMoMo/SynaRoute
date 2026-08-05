import { useMemo } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";

export interface TocItem {
  id: string;
  text: string;
  level: 2 | 3;
}

export interface RenderedMarkdown {
  html: string;
  toc: TocItem[];
}

/** 标题文本 → 锚点 id。中文标题不做转写，直接保留（现代浏览器与 URL 都支持） */
function slugify(text: string, used: Set<string>): string {
  const base =
    text
      .trim()
      .toLowerCase()
      .replace(/[`*_~]/g, "")
      .replace(/[^\p{L}\p{N}\s-]/gu, "")
      .replace(/\s+/g, "-")
      .replace(/^-+|-+$/g, "") || "section";

  let id = base;
  let n = 2;
  while (used.has(id)) id = `${base}-${n++}`;
  used.add(id);
  return id;
}

export interface MarkdownOptions {
  /**
   * 标题降级层数。
   *
   * 更新日志页里，页面 h1 是「更新日志」、每个版本号是 h2，此时发布说明正文里的
   * `##` 若仍渲染成 h2，就会与版本号平级 —— 读屏和搜索引擎都会把「修复」「新增」
   * 这些小节当成与版本号同级的独立章节。传 1 让正文整体降一级即可对齐真实层级。
   *
   * 文档页与条款页不需要（它们的 `#` 就是页面唯一的 h1），默认 0。
   */
  headingOffset?: number;
}

/**
 * 把 Markdown 渲染成可直接插入的 HTML，并顺带抽出目录。
 *
 * 关于 `dangerouslySetInnerHTML`：模板第 19 节允许「有明确且安全的内容来源」时使用。
 * 这里两个来源分别是仓库内自己写的 .md 文件，以及 GitHub 发布说明。
 * 前者完全可控，后者是自己发的版本说明 —— 但仍然过一遍 DOMPurify 再插入，
 * 因为「内容可信」和「内容不含可执行标记」是两回事，清洗的成本极低。
 */
export function useMarkdown(source: string, options: MarkdownOptions = {}): RenderedMarkdown {
  const { headingOffset = 0 } = options;

  return useMemo(() => {
    const used = new Set<string>();
    const toc: TocItem[] = [];

    const renderer = new marked.Renderer();
    // 给 h2/h3 加锚点 id，右侧目录靠它跳转
    renderer.heading = ({ tokens, depth }) => {
      const text = tokens.map((t) => ("raw" in t ? String(t.raw) : "")).join("");
      const plain = text.replace(/[`*_~]/g, "");
      // 降级后仍钳在 h6 以内，否则会渲染出 <h7> 这种不存在的标签
      const level = Math.min(6, depth + headingOffset);
      if (depth === 2 || depth === 3) {
        const id = slugify(plain, used);
        toc.push({ id, text: plain, level: depth });
        return `<h${level} id="${id}">${marked.parseInline(text) as string}</h${level}>`;
      }
      return `<h${level}>${marked.parseInline(text) as string}</h${level}>`;
    };
    // 表格外面包一层可横向滚动的容器，否则宽表格会把移动端整页撑宽
    renderer.table = ({ header, rows }) => {
      const headHtml = header.map((c) => `<th>${marked.parseInline(c.text) as string}</th>`).join("");
      const bodyHtml = rows
        .map((row) => `<tr>${row.map((c) => `<td>${marked.parseInline(c.text) as string}</td>`).join("")}</tr>`)
        .join("");
      return `<div class="table-wrap"><table><thead><tr>${headHtml}</tr></thead><tbody>${bodyHtml}</tbody></table></div>`;
    };

    const raw = marked.parse(source, { renderer, async: false, gfm: true, breaks: false }) as string;

    const html = DOMPurify.sanitize(raw, {
      ADD_ATTR: ["target", "rel", "id"],
      // 站内文档不需要任何脚本/样式/表单元素
      FORBID_TAGS: ["script", "style", "iframe", "form", "input", "button"],
    });

    return { html, toc };
  }, [source, headingOffset]);
}

/** 文档正文里的外链在新标签打开，且必须带安全属性 */
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node instanceof HTMLAnchorElement && node.hasAttribute("href")) {
    const href = node.getAttribute("href") ?? "";
    if (/^https?:\/\//i.test(href)) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
  }
});
