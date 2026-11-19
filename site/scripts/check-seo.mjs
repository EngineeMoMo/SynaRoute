import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export const origin = "https://synaroute.mofamilys.com";
const englishHome = `${origin}/en`;

function attributes(markup, tagName) {
  const tags = markup.matchAll(new RegExp(`<${tagName}\\s[^>]*>`, "gi"));
  return [...tags].map(([tag]) => {
    const attrs = {};
    for (const [, name, doubleQuoted, singleQuoted] of tag.matchAll(/\s+([\w:-]+)=(?:"([^"]*)"|'([^']*)')/g)) {
      attrs[name.toLowerCase()] = doubleQuoted ?? singleQuoted;
    }
    return attrs;
  });
}

function checkAlternates(links, url) {
  const suffix = new URL(url).pathname.slice(3);
  const expected = { zh: `${origin}/zh${suffix}`, en: `${englishHome}${suffix}`, "x-default": `${englishHome}${suffix}` };
  const alternates = links.filter((link) => link.rel === "alternate" && link.hreflang);
  assert.equal(alternates.length, 3, `${url} 语言链接数量有误`);
  assert.deepEqual(Object.fromEntries(alternates.map((link) => [link.hreflang, link.href])), expected, `${url} 语言链接目标有误`);
}

function inspectPage(html, url) {
  const canonical = url === `${origin}/` ? englishHome : url;
  const head = html.match(/<head\b[^>]*>([\s\S]*?)<\/head>/i)?.[1] ?? "";
  const main = html.match(/<main\b[^>]*>([\s\S]*?)<\/main>/i)?.[1] ?? "";
  assert.match(main, /<h1\b/, "静态正文缺少主标题");
  assert.doesNotMatch(main, /aria-busy="true"/, "静态正文仍在加载");
  assert.equal(attributes(html, "html")[0]?.lang, new URL(canonical).pathname.startsWith("/zh") ? "zh-CN" : "en", "页面语言与网址不一致");
  const links = attributes(head, "link");
  assert.deepEqual(links.filter((link) => link.rel === "canonical").map((link) => link.href), [canonical], "规范网址有误");
  checkAlternates(links, canonical);
  const meta = attributes(head, "meta");
  assert.ok(!meta.some((tag) => tag["http-equiv"]?.toLowerCase() === "refresh"), "页面包含刷新重定向");
  for (const tag of meta.filter((tag) => /^(robots|googlebot)$/i.test(tag.name))) {
    assert.doesNotMatch(tag.content ?? "", /\b(noindex|none)\b/i, "页面禁止索引");
  }
  const title = head.match(/<title>([^<]+)<\/title>/i)?.[1].trim();
  const description = meta.find((tag) => tag.name === "description")?.content?.trim();
  assert.ok(title && description, "缺少标题或描述");
  return { title, description, anchors: attributes(html, "a").map((link) => link.href).filter(Boolean) };
}

export async function checkPublicSite(read) {
  const robots = await read("/robots.txt");
  assert.ok(robots.includes(`Sitemap: ${origin}/sitemap.xml`), "robots.txt 须声明规范 sitemap");
  assert.match(robots, /^User-agent:\s*\*\s*$/im, "缺少默认抓取规则");
  assert.doesNotMatch(robots, /^Disallow:\s*\/\s*$/im, "robots.txt 阻止整站抓取");
  const sitemap = await read("/sitemap.xml");
  const urls = [...sitemap.matchAll(/<url>([\s\S]*?)<\/url>/g)].map(([, entry]) => {
    const url = entry.match(/<loc>([^<]+)<\/loc>/)?.[1];
    assert.ok(url, "sitemap 缺少 loc");
    const parsed = new URL(url);
    assert.equal(parsed.origin, origin, "sitemap 只能包含官网网址");
    assert.match(parsed.pathname, /^\/(zh|en)(?:\/[\w-]+)*$/, `${url} 须使用无扩展名的规范路径`);
    assert.equal(`${origin}${parsed.pathname}`, url, "sitemap 含非规范网址");
    checkAlternates(attributes(entry, "xhtml:link"), url);
    return url;
  });
  assert.ok(urls.includes(englishHome) && urls.includes(`${origin}/zh`), "sitemap 缺少中英文首页");
  assert.equal(new Set(urls).size, urls.length, "sitemap 网址重复");
  for (const url of urls) {
    const path = new URL(url).pathname;
    const counterpart = `${origin}/${path.startsWith("/zh") ? "en" : "zh"}${path.slice(3)}`;
    assert.ok(urls.includes(counterpart), `${url} 在 sitemap 中缺少另一语言版本`);
  }

  const root = `${origin}/`;
  const graph = new Map();
  const titles = new Set();
  const descriptions = new Set();
  const errors = [];
  for (const url of [root, ...urls]) {
    try {
      const page = inspectPage(await read(new URL(url).pathname), url);
      graph.set(url, page.anchors.map((href) => {
        const target = new URL(href, url);
        return `${target.origin}${target.pathname}`;
      }));
      if (url !== root) {
        assert.ok(!titles.has(page.title), `页面标题重复：${page.title}`);
        assert.ok(!descriptions.has(page.description), "页面描述重复");
        titles.add(page.title);
        descriptions.add(page.description);
      }
    } catch (error) {
      errors.push(`${url} — ${error.message}`);
    }
  }
  if (graph.size === urls.length + 1) {
    const reached = new Set([root]);
    for (const url of reached) {
      for (const target of graph.get(url)) {
        if (graph.has(target)) reached.add(target);
      }
    }
    const missing = urls.filter((url) => !reached.has(url));
    if (missing.length) errors.push(`从根首页沿 HTML 链接无法到达：${missing.join(", ")}`);
  }
  if (errors.length) throw new Error(errors.join("\n"));
  return urls.length;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const dist = new URL("../dist/", import.meta.url);
  const read = (file) => readFileSync(new URL(file, dist), "utf8");
  const notFound = read("404.html");
  assert.doesNotMatch(notFound, /http-equiv="refresh"/i, "404 不应自动跳转");
  assert.match(notFound, /<meta[^>]+name="robots"[^>]+content="noindex,follow"/, "404 应禁止索引");
  assert.ok(read("index.html") === read("en.html"), "根网址与英文首页应提供相同正文");
  const count = await checkPublicSite((path) => {
    if (path === "/") return read("index.html");
    const relative = path.slice(1);
    if (path === "/robots.txt" || path === "/sitemap.xml") return read(relative);
    const html = read(`${relative}.html`);
    assert.ok(html === read(`${relative}/index.html`), `${path} 的两份预渲染产物应完全相同`);
    return html;
  });
  console.log(`[check-seo] 根首页、404 及 ${count} 条规范网址的正文、语言链接与首页可达性验证通过`);
}
