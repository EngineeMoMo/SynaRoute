import type { Lang } from "@/i18n";

// 文档正文以 Markdown 原文入库，`?raw` 拿到字符串后在页面里渲染。
// 用 Markdown 而不是写成 TSX，是为了让文档能从仓库 docs/ 下的原文直接改写过来，
// 后续更新只改 .md，不用碰组件。
import cliZh from "@/content/zh/cli.md?raw";
import cliEn from "@/content/en/cli.md?raw";
import brainZh from "@/content/zh/brain.md?raw";
import brainEn from "@/content/en/brain.md?raw";
import mcpZh from "@/content/zh/mcp.md?raw";
import mcpEn from "@/content/en/mcp.md?raw";

export interface DocEntry {
  /** URL 里的 slug：/zh/docs/<slug> */
  slug: string;
  /** 取词前缀：`${i18nPrefix}.title` / `.desc` */
  i18nPrefix: string;
  /** 各语言正文 */
  body: Record<Lang, string>;
  /** 仓库里的原文路径，文档页底部给一个「查看原文」链接 */
  sourcePath: string;
}

export const docs: DocEntry[] = [
  {
    slug: "cli",
    i18nPrefix: "docs.cli",
    body: { zh: cliZh, en: cliEn },
    sourcePath: "docs/12-CLI用户手册.md",
  },
  {
    slug: "brain",
    i18nPrefix: "docs.brain",
    body: { zh: brainZh, en: brainEn },
    sourcePath: "docs/10-大脑聚合使用说明.md",
  },
  {
    slug: "mcp",
    i18nPrefix: "docs.mcp",
    body: { zh: mcpZh, en: mcpEn },
    sourcePath: "docs/06-MCP使用手册.md",
  },
];

export function findDoc(slug: string | undefined): DocEntry | undefined {
  return docs.find((d) => d.slug === slug);
}
