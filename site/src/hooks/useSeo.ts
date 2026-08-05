import { useEffect } from "react";
import { useLocation } from "react-router-dom";
import { siteConfig } from "@/config/site";
import { LANGS, type Lang } from "@/i18n";

export interface SeoInput {
  title: string;
  description: string;
  lang: Lang;
  /** 社交分享图，相对站点根，默认取 siteConfig.ogImage */
  image?: string;
  /** 文章类页面（文档、更新日志）标 article，其余标 website */
  type?: "website" | "article";
}

/** 找到或建一个 <meta>，避免每次导航往 head 里堆重复标签 */
function upsertMeta(selector: string, attrs: Record<string, string>) {
  let el = document.head.querySelector<HTMLMetaElement>(selector);
  if (!el) {
    el = document.createElement("meta");
    document.head.appendChild(el);
  }
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
}

function upsertLink(rel: string, href: string, extra: Record<string, string> = {}) {
  const key = extra.hreflang ? `link[rel="${rel}"][hreflang="${extra.hreflang}"]` : `link[rel="${rel}"]`;
  let el = document.head.querySelector<HTMLLinkElement>(key);
  if (!el) {
    el = document.createElement("link");
    el.rel = rel;
    document.head.appendChild(el);
  }
  el.href = href;
  for (const [k, v] of Object.entries(extra)) el.setAttribute(k, v);
}

/**
 * 按路由更新页面 meta。
 *
 * 刻意不引 react-helmet 之类的库（模板第 10.1 节：如无必要不用大型库）——
 * 需要的只是往 head 里写十来个标签，直接操作 DOM 更小更直白。
 *
 * ⚠️ 已知局限：这是客户端渲染，meta 要等 JS 执行后才出现。Google 会执行 JS，
 * 但部分抓取器（含一些社交平台的分享卡片抓取）只读初始 HTML。
 * index.html 里因此预置了一份默认的 title/description/OG 作为兜底。
 */
export function useSeo({ title, description, lang, image, type = "website" }: SeoInput) {
  const { pathname } = useLocation();

  useEffect(() => {
    const fullTitle = title === siteConfig.name ? title : `${title} · ${siteConfig.name}`;
    const canonical = `${siteConfig.url}${pathname}`;
    const ogImage = `${siteConfig.url}${image ?? siteConfig.ogImage}`;

    document.title = fullTitle;
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";

    upsertMeta('meta[name="description"]', { name: "description", content: description });

    upsertMeta('meta[property="og:title"]', { property: "og:title", content: fullTitle });
    upsertMeta('meta[property="og:description"]', { property: "og:description", content: description });
    upsertMeta('meta[property="og:type"]', { property: "og:type", content: type });
    upsertMeta('meta[property="og:url"]', { property: "og:url", content: canonical });
    upsertMeta('meta[property="og:image"]', { property: "og:image", content: ogImage });
    upsertMeta('meta[property="og:site_name"]', { property: "og:site_name", content: siteConfig.name });
    upsertMeta('meta[property="og:locale"]', {
      property: "og:locale",
      content: lang === "zh" ? "zh_CN" : "en_US",
    });

    upsertMeta('meta[name="twitter:card"]', { name: "twitter:card", content: "summary_large_image" });
    upsertMeta('meta[name="twitter:title"]', { name: "twitter:title", content: fullTitle });
    upsertMeta('meta[name="twitter:description"]', { name: "twitter:description", content: description });
    upsertMeta('meta[name="twitter:image"]', { name: "twitter:image", content: ogImage });

    upsertLink("canonical", canonical);

    // hreflang：把同一页面的另一语言版本告诉搜索引擎，避免被判重复内容
    const rest = pathname.split("/").filter(Boolean).slice(1).join("/");
    for (const { value } of LANGS) {
      upsertLink("alternate", `${siteConfig.url}/${value}${rest ? `/${rest}` : ""}`, { hreflang: value });
    }
    upsertLink("alternate", `${siteConfig.url}/zh${rest ? `/${rest}` : ""}`, { hreflang: "x-default" });
  }, [title, description, lang, image, type, pathname]);
}
