// 验证实际发布产物，防止根网址重新退回空壳或内容页继承根跳转。
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const dist = new URL("../dist/", import.meta.url);
const read = (path) => readFileSync(new URL(path, dist), "utf8");
const origin = "https://synaroute.mofamilys.com";
const home = `${origin}/zh`;
const root = read("index.html");
assert.match(root, /<meta http-equiv="refresh" content="0; url=https:\/\/synaroute\.mofamilys\.com\/zh"\s*\/>/);
assert.ok(root.includes(`<link rel="canonical" href="${home}"`), "根网址须指向固定规范首页");
assert.ok(root.includes(`<a href="${home}">`), "根网址须提供无需 JS 的首页链接");
assert.doesNotMatch(root, /<script\b/i, "根跳转不能依赖或竞争于 JS 语言检测");
assert.doesNotMatch(read("404.html"), /http-equiv="refresh"/i, "404 兜底不可继承首页跳转");

const urls = [...read("sitemap.xml").matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
assert.ok(urls.includes(home), "sitemap 必须包含跳转目标");
assert.ok(!urls.includes(`${origin}/`), "跳转入口不应加入 sitemap");
for (const url of urls) {
  assert.equal(new URL(url).origin, origin);
  const path = new URL(url).pathname.slice(1);
  for (const file of [`${path}.html`, `${path}/index.html`]) {
    const html = read(file);
    assert.match(html, /<main\b/, `${file} 缺少预渲染正文`);
    assert.match(html, /<h1\b/, `${file} 缺少页面主标题`);
    assert.ok(html.includes(`<link rel="canonical" href="${url}"`), `${file} canonical 与 sitemap 不一致`);
    assert.doesNotMatch(html, /http-equiv="refresh"/i, `${file} 不应跳转`);
    assert.doesNotMatch(html, /<meta[^>]+name="robots"[^>]+content="[^"]*noindex/i, `${file} 不应禁止索引`);
  }
}
console.log(`[check-seo] 根跳转、404 兜底和 ${urls.length} 条 sitemap URL 的双份预渲染通过`);
