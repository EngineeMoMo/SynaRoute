# 产品截图采集步骤

官网的 5 张截图取自**桌面应用的浏览器预览模式**，不是真机运行截图。

这样做的原因：预览模式走 `src/lib/mockData.ts` 的示例数据（`relay.example.com`、
`GLM5.1 → opus-4-7` 之类），界面是真的、数据是假的，**不存在泄露真实密钥或真实厂商地址的风险**。
用真机截图则每张都要人工核对有没有漏掉敏感信息。

## 路由功能补图（2026-11-07）

新增 `routing-overview.jpg`、`routing-scenes.jpg`、`routing-affinity.jpg`、`routing-intent.jpg`、`routing-privacy.jpg`，用于首页路由说明与中英文 CLI 指南。取自当前应用浏览器预览，保留示例/草稿状态，不含真实配置。这批是单套中文界面截图，不冒充旧图库的1440×900明暗双版本；首页以折叠示例呈现，点击原图放大。场景和意图图展示未保存编辑入口，不表示策略已启用。

## 采集

1. 在**仓库根目录**起前端 dev server（不是 `site/`）：

   ```bash
   npm run dev
   ```

   打开 http://localhost:1420 ，界面顶部应显示「浏览器预览模式」提示条 —— 看到它才说明走的是 mock 数据。

2. 把浏览器窗口/视口调成 **1440 × 900**（与 `site/src/data/screenshots.ts` 里的
   `SHOT_WIDTH` / `SHOT_HEIGHT` 一致；改了那两个常量就要按新尺寸重拍全部）。

3. 逐个页面截图，**明暗各一张**（左上角有主题切换）。文件名必须与
   `screenshots.ts` 里登记的一致：

   | 页面 | 浅色文件名 | 深色文件名 |
   |---|---|---|
   | Claude CLI 分类页 | `category-light.png` | `category-dark.png` |
   | 大脑聚合 | `brain-light.png` | `brain-dark.png` |
   | 运行日志 | `logs-light.png` | `logs-dark.png` |
   | 设置 | `settings-light.png` | `settings-dark.png` |
   | 厂商管理 | `vendors-light.png` | `vendors-dark.png` |

4. 全部放进 `site/public/screenshots/`。

## 采集前的检查

- 顶部**必须**有「浏览器预览模式」提示条。没有它说明连的是真实后端，画面里可能有真实数据。
- 侧栏语言切到中文（官网中英两版共用同一批图，中文界面对主要受众更直观）。
- 截图里不要出现浏览器地址栏、书签栏、开发者工具。

## 缺图时会怎样

`site/src/components/ui/Screenshot.tsx` 在图片加载失败时渲染一个标注「产品截图占位」的
占位块，**不会出现破图**，也不会伪造界面。所以图没到位站点仍可正常发布，只是展示效果打折。

## 换了界面之后

改过 UI 就要重拍对应那张，否则官网展示的是旧界面。判据很简单：
`site/public/screenshots/` 里每个文件名都能在 `screenshots.ts` 里找到登记，反之亦然。
