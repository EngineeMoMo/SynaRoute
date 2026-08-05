# SynaRoute 官网

`synaroute.mofamilys.com` 的源码。与桌面应用同仓但**完全独立**：自己的 `package.json`、
自己的 `node_modules`、自己的构建产物，改这里不影响应用的任何构建或测试。

## 本地开发

```bash
cd site
npm install
npm run dev
```

开发服务器在 **1430** 端口（应用的 dev server 固定占 1420，两边可同时开）。

```bash
npm run typecheck   # tsc --noEmit
npm run build       # tsc + vite build + 构建后处理
npm run preview     # 本地预览生产产物
```

## 设计令牌从哪来

`site/tailwind.config.js` 直接 `import ../tailwind.config.js`，展开应用的 `theme.extend`。
主色、圆角、阴影、字体，以及 `opacity: {8, 12}` 那两档都是**复用**而非复制 ——
改应用主题，官网自动跟随。

唯一抄了一份的是 `src/styles.css` 里的 CSS 变量块（`:root` / `.dark`）。
原因是应用的基础样式含 `body { overflow: hidden }`（桌面端整页不滚动），
网站整体引入会直接滚不动。改应用配色时记得同步这一处。

判据：`npm run build` 后 `dist/assets/*.css` 里应能搜到 `.bg-warning\/8` 与 `var(--primary)`。

## 目录

```
src/
├── config/site.ts      产品名 / 域名 / GitHub / 邮箱 / 统计开关（单一事实来源）
├── data/               平台矩阵、功能、截图、FAQ、导航 —— 只存 i18n key，不存文案
├── i18n/               中英文案字典（扁平 key，zh 为基准，en 缺词回退 zh）
├── content/{zh,en}/    文档与条款正文（Markdown，?raw 导入后渲染）
├── hooks/              取词 / 主题 / SEO / GitHub Release / 进入视口 / Markdown
├── components/
│   ├── layout/         Header · Footer · BackToTop
│   ├── sections/       首页各区块
│   ├── ui/             Button · Section · Logo · Accordion · Lightbox · Screenshot
│   └── DownloadUI.tsx  下载相关组件（Hero 按钮 / 平台徽标 / 平台卡片）
└── pages/              Home · Download · Docs · Doc · Changelog · Legal · NotFound
```

## 加 macOS 版下载

**只改数据，不改组件。** `src/data/platforms.ts` 里把 macOS 那条的
`status` 从 `"coming-soon"` 改成 `"available"`，确认 `assetPattern` 能匹配到
Release 里的 dmg 资产名即可。

Hero 的平台徽标、首页下载区、下载页三处都读同一份数据，会同时生效。
`coming-soon` 的平台**在 DOM 里根本不生成 href**，所以不存在「点了跳 404」。

## 版本号与下载链接

不硬编码。运行时匿名调 GitHub API 取最新 Release 的 tag 与资产直链，
会话内缓存（匿名限流 60 次/小时/IP）。

任何失败路径都有兜底：拿不到就用 `config/site.ts` 里的 `fallbackVersion`，
下载按钮回落到 `releases/latest` 页面。**官网不会因为 GitHub 抽风而变成没有下载入口的页面。**

发新版后可以顺手更新 `fallbackVersion`，不更新也不会产生坏链。

## 截图

见 [scripts/capture-shots.md](scripts/capture-shots.md)。图片缺失时会渲染标注
「产品截图占位」的占位块，不会出现破图。

## 部署

推到 `master` 且 `site/**` 有变动时，`.github/workflows/deploy-site.yml`
自动构建并发布到 GitHub Pages。

仓库侧需要一次性配置（只做一次）：

1. GitHub → Settings → Pages → Source 选 **GitHub Actions**
2. Custom domain 填 `synaroute.mofamilys.com`，勾 Enforce HTTPS
3. 域名商处加 CNAME 解析：`synaroute` → `engineemomo.github.io`

`public/CNAME` 已在仓库里，构建时会复制进产物 —— 少了它每次部署都会把自定义域名清掉。

### 换回 GitHub Pages 默认域名

三处一起改：`vite.config.ts` 的 `base` 改成 `"/SynaRoute/"`、
`config/site.ts` 的 `url`、`scripts/postbuild.mjs` 的 `SITE_URL`，并删掉 `public/CNAME`。

## 文案准则

官网只写能在代码里取证的事实。具体禁区写在 `src/i18n/zh.ts` 顶部的注释里，
要点是：不写「绝对安全」、不虚构安全审计与隐私认证、不虚构用户数与下载量；
仓库虽是 public 但**没有 LICENSE 文件**，因此一律表述为「源码公开」而非「开源」。
