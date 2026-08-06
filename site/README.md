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
npm run build       # tsc + vite build + 透明度体检 + 构建后处理
npm run preview     # 本地预览生产产物
npm run icons       # 重生成 favicon / logo / OG 图（改了品牌图形才需要跑）
```

## 设计令牌从哪来

`site/tailwind.config.js` 直接 `import ../tailwind.config.js`，展开应用的 `theme.extend`。
主色、圆角、阴影、字体，以及 `opacity: {8, 12}` 那两档都是**复用**而非复制 ——
改应用主题，官网自动跟随。

唯一抄了一份的是 `src/styles.css` 里的 CSS 变量块（`:root` / `.dark`）。
原因是应用的基础样式含 `body { overflow: hidden }`（桌面端整页不滚动），
网站整体引入会直接滚不动。改应用配色时记得同步这一处。

### 官网与应用有意不同的三处颜色

不要「统一」回去，每一处都是量出来的：

| Token | 官网值 | 应用值 | 为什么 |
|---|---|---|---|
| `--text-secondary`（浅色） | `#64646E` | `#71717A` | 应用值在纯白上是 4.83:1，勉强过 4.5。官网有 `primary/8`、`warning/8`、`surface-hover` 这些淡底纹区块，底色一浅立刻掉到 3.89~4.40 全线不达标。深一档后在这些底纹上仍有 4.98~5.34 |
| `--primary-solid` | 新增 token | 无 | 白字压在实心主色块上要 4.5:1。深色模式把 `--primary` 提亮成 `#8174FF` 是为了「主色当文字画在近黑底上」，白字压这个亮紫只有 **3.57:1**。两种用法诉求相反，只能拆成两个 token。**所有 `text-primary-foreground` 的底色必须用 `bg-primary-solid`** |
| `text-muted` 的用途 | 仅装饰 | 次要标签 | `#A1A1AA` on 白 = 2.56:1，深色 `#52525B` on `#18181B` = 2.20:1。官网正文层级靠**字号 + 字重**分级，不用第三档灰。详见 `styles.css` 顶部 |

对比度是用「合成后的实际底色」量的 —— 淡底纹是半透明的，必须与祖先逐层
alpha 合成才算得对，直接把 `rgba(109,94,247,0.08)` 当实色算会得出 1.0 这种假数字。

### 透明度修饰符是会静默失效的

Tailwind 3.4 的 `opacity` 标度默认只认 5 的倍数。写 `bg-warning/8` **不报错、
也一条 CSS 都不生成** —— 表现是「有边框有字色、独独没底色」，肉眼极难发现。
应用配置里补了 8 / 12 两档，官网继承过来。

`npm run build` 会跑 `scripts/check-opacity.mjs`：扫源码里所有 `xx-yy/nn` 类名，
逐个到产物 CSS 里核对是否真的生成了规则，缺一个就让构建失败。新用一个不在标度里的
档位（比如 `/7`）会立刻在构建时报出来，而不是等上线后看着「颜色好像不太对」猜半天。

判据：`npm run build` 后 `dist/assets/*.css` 里应能搜到 `.bg-warning\/8` 与 `var(--primary)`。

## 排版核对（拍图看）

改完首页排版别只靠 DOM 断言，拍出来看一眼：

```bash
npm run dev                                  # 另一个终端
node scripts/review-sections.mjs zh light    # 按区块拍，一屏一张
node scripts/review-sections.mjs zh dark
node scripts/review-shots.mjs                # 中英 × 明暗 四张全页长图
```

产物写到 `.shots-review/`（点目录，`postbuild.mjs` 会硬失败拦住任何点目录进 dist，
不会误发布）。脚本会先把 `.reveal` 全部置为可见、把懒加载图改成 eager 并滚一遍页面，
否则拍到的是一片空白（进入视口动画还没触发）。

## 品牌图形

标记（一条对角线串三个节点）有两份实现，**改图形要两边一起改**：

- `src/components/ui/Logo.tsx` —— 站内内联 SVG，任意尺寸清晰、深色模式可换色
- `scripts/gen-favicon.mjs` —— 生成 `favicon.ico`(16+32) / `favicon-32.png` / `logo-256.png`

生成脚本自己写了个 PNG/ICO 编码器（只用 `node:zlib`），因为仓库里没有 sharp/canvas
这类原生依赖，为几个图标装一个要编译的包不值当。脚本内置判据：四角 alpha 必须为 0
（圆角真的生效）、中心 alpha 必须为 255（图形真的画上了），不满足直接退出。

`scripts/gen-og.mjs` 同理，生成 1200×630 的社交分享图。两个脚本都在 `npm run icons` 里。

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

## 首页区块顺序与排版约束

`src/pages/HomePage.tsx` 里的顺序是刻意的：Hero → Benefits →
**BrainSpotlight** → Features → Screenshots → Downloads → GettingStarted →
Security → FAQ → FinalCTA。

大脑聚合单独成区、紧跟 Benefits，不在 Features 的九宫格里。原因：那是本产品唯一
「别处没有」的能力，塞进等大的卡片阵列和「托盘与自启动」同权重，等于把最强的差异点
藏起来了。它是全页唯一换背景色的区块。

几处**排满整数行**的约束，加内容时别破坏（否则最后一行会剩一张孤零零的卡，看着像漏排）：

| 位置 | 约束 |
|---|---|
| `data/features.ts` | 2 个 `half` + 6 个 `third`（两列一行 + 三列两行）。成对加 half，或三个一组加 third |
| `sections/Security.tsx` | `facts` 六条走三列两行；风险提示单独整行、不进网格 |
| `data/faq.ts` | 十条，独立卡片纵向排列，无行数约束 |

Hero 标题拆成 `hero.titleLead` + `hero.titleTail` 两个 key。中文两段**各自**套
`whitespace-nowrap`（中文无词边界，不锁会断出「多个 Key 互为备 / 份，」这种半个词），
英文**不锁**。切分点就是那个逗号，改文案时保持这个结构。

因为中文段锁了 nowrap 就不可压缩，字号需要三档：`hero-xs`(30px) / `hero-sm`(36px, ≥360px)
/ `hero`(56px, ≥640px)。36px 下最长那段要 325px，而 320px 视口的容器只有 280px ——
不降档会被 Hero section 的 `overflow-hidden` 直接裁掉右半句。为此在 tailwind.config.js
补了一个 `xs: 360px` 断点（Tailwind 默认最小是 sm=640）。

## 加 macOS 版下载

**只改数据，不改组件。** `src/data/platforms.ts` 里把 macOS 那条的
`status` 从 `"coming-soon"` 改成 `"available"`，确认 `assetPattern` 能匹配到
Release 里的 dmg 资产名即可。

Hero 的平台徽标、首页下载区、下载页三处都读同一份数据，会同时生效。
`coming-soon` 的平台**在 DOM 里根本不生成 href**，所以不存在「点了跳 404」。

安装包格式文案走 i18n（`platform.<id>.format`），不写在 platforms.ts 里 ——
「NSIS 安装包」需要翻译，`minOS` 那种两种语言写法一致的才留在数据里。

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
