// 验证实际发布产物，防止根网址重新退回空壳或内容页继承根跳转。
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const dist = new URL("../dist/", import.meta.url);
const read = (path) => readFileSync(new URL(path, dist), "utf8");
const origin = "https://synaroute.mofamilys.com";
const home = `${origin}/en`;
const root = read("index.html");
assert.match(root, /<main\b/, "根网址须直接提供预渲染正文");
assert.match(root, /<h1\b/, "根网址须直接提供页面主标题");
assert.match(root, /<html\b[^>]*\blang="en"/, "根网址须默认使用英文");
assert.doesNotMatch(root, /http-equiv="refresh"/i, "根网址不可再使用中转跳转页");
assert.ok(root.includes(`<link rel="canonical" href="${home}"`), "根网址须指向固定规范首页");
assert.equal(root, read("en.html"), "根网址须与规范英文首页提供相同完整内容");
assert.doesNotMatch(read("404.html"), /http-equiv="refresh"/i, "404 兜底不可继承首页跳转");

const sitemap = read("sitemap.xml");
const urls = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
assert.ok(urls.includes(home), "sitemap 必须包含规范英文首页");
assert.ok(!urls.includes(`${origin}/`), "非规范首页别名不应重复加入 sitemap");
for (const url of urls) {
  assert.equal(new URL(url).origin, origin);
  const path = new URL(url).pathname.slice(1);
  const defaultUrl = `${home}${path.slice(2)}`;
  assert.ok(sitemap.includes(`hreflang="x-default" href="${defaultUrl}"`), `${url} 的 sitemap 默认语言须指向英文`);
  for (const file of [`${path}.html`, `${path}/index.html`]) {
    const html = read(file);
    assert.match(html, /<main\b/, `${file} 缺少预渲染正文`);
    assert.match(html, /<h1\b/, `${file} 缺少页面主标题`);
    assert.ok(html.includes(`<link rel="canonical" href="${url}"`), `${file} canonical 与 sitemap 不一致`);
    assert.match(html, new RegExp(`<html\\b[^>]*\\blang="${path.startsWith("zh") ? "zh-CN" : "en"}"`), `${file} 的页面语言与 URL 不一致`);
    assert.ok(html.includes(`<link rel="alternate" href="${defaultUrl}" hreflang="x-default"`), `${file} 的默认语言须指向英文`);
    assert.doesNotMatch(html, /http-equiv="refresh"/i, `${file} 不应跳转`);
    assert.doesNotMatch(html, /<meta[^>]+name="robots"[^>]+content="[^"]*noindex/i, `${file} 不应禁止索引`);
  }
}
console.log(`[check-seo] 根网址正文直出、404 兜底和 ${urls.length} 条 sitemap URL 的双份预渲染通过`);
