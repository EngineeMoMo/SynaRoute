import appConfig from "../tailwind.config.js";

/**
 * 官网 Tailwind 配置。
 *
 * 设计令牌（颜色 / 圆角 / 阴影 / 字体 / opacity 8·12 两档）直接从桌面应用的
 * tailwind.config.js 复用，**不复制颜色值** —— 改应用主色，官网自动跟随，
 * 不会出现「软件是靛紫、官网是另一种紫」的漂移。
 *
 * 注意：应用配置里 `opacity: {8, 12}` 那两档是修 Tailwind 3.4 透明度标度只认 5 的
 * 倍数的坑（`bg-warning/8` 不报错但一条 CSS 都不生成）。这里一并继承，官网用同样
 * 的浅色底纹写法才不会静默失效。
 *
 * @type {import('tailwindcss').Config}
 */
export default {
  darkMode: ["class"],
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    /**
     * 补一个 360px 的 `xs` 断点（Tailwind 默认最小是 sm=640）。
     * 用途只有一个：Hero 标题在 320~359px 这段要更小的字号，理由见下面 hero-xs。
     * 其余断点沿用默认，不动。
     */
    screens: {
      xs: "360px",
      sm: "640px",
      md: "768px",
      lg: "1024px",
      xl: "1280px",
      "2xl": "1536px",
    },
    extend: {
      ...appConfig.theme.extend,

      // ---- 以下为官网独有，桌面应用不需要 ----

      colors: {
        ...appConfig.theme.extend.colors,
        /**
         * 实心按钮/徽标的底色，白字压在上面。
         *
         * 为什么不直接用 `primary`：深色模式把 `--primary` 提亮成 #8174FF 是为了
         * 「主色当文字画在近黑底上」，而白字压在这个亮紫上只有 3.57:1，达不到 AA。
         * 两种用法的诉求相反，只能分成两个 token。浅色模式两者同值，观感不变。
         * 见 styles.css 里 --primary-solid 的注释。
         */
        "primary-solid": "rgb(var(--primary-solid) / <alpha-value>)",
        /** 描边再重一档，只用于可点卡片的 hover。见 styles.css 里 --border-strong。 */
        "border-strong": "rgb(var(--border-strong) / <alpha-value>)",
      },

      /**
       * 🔴 覆盖应用那份的 `boxShadow`（**不是**去改应用的 —— 桌面应用共用它，
       * 且它那两档在应用的信息密集型界面里是合适的）。
       *
       * 官网这边必须覆盖，因为应用那两档在官网上等于不存在：
       * - 浅色：`rgba(0,0,0,.04)` 的 1px 投影压在 #FFF-on-#FAFAFA 上（两色本身
       *   只差 1.04:1），肉眼看不出卡片有没有抬起；
       * - 深色：黑影压在 #0E0E11 上算出来差 0.6/255，**一条 CSS 都没写**，
       *   于是全站 30 余处 `hover:shadow-card-hover` 在深色下毫无反馈。
       *
       * 改成读 CSS 变量，两个主题各给一组（值与理由在 styles.css）。
       * 类名保持 `shadow-card` / `shadow-card-hover` 不变 —— 30 余处调用点
       * 不用动，自动升级；新增的 `shadow-raised` 只给「浮在页面之上」的元素。
       *
       * 判据：`npm run build` 后 dist CSS 里 `.shadow-card` 的值应是
       * `var(--shadow-card)`，且 styles.css 的 `:root` 与 `.dark` 里都能搜到它。
       */
      boxShadow: {
        card: "var(--shadow-card)",
        "card-hover": "var(--shadow-card-hover)",
        raised: "var(--shadow-raised)",
      },

      maxWidth: {
        // 正文主容器宽度：模板建议 1120~1280，取中间值兼顾大屏留白与信息密度
        content: "1200px",
        /**
         * 文档 / 条款正文的单列宽度。
         *
         * 🔴 原先是 `72ch`，而 `ch` 是**相对所在元素字号**的单位 —— 同一个
         * `max-w-prose` 在条款页（16px 上下文）解析成 625px、在文档页
         * （`.prose-doc` 的 15px）解析成 583px，两页拿到两个不同的行宽而
         * 类名一模一样。改成固定值，两页真正一致。
         */
        prose: "640px",
      },
      fontSize: {
        // Hero 主标题：桌面 56px / 移动 36px（由响应式类切换），行高压紧显紧凑
        hero: ["3.5rem", { lineHeight: "1.1", letterSpacing: "-0.02em" }],
        "hero-sm": ["2.25rem", { lineHeight: "1.15", letterSpacing: "-0.02em" }],
        /**
         * 320px 这类极窄屏专用。
         *
         * 中文标题的两段各自套了 whitespace-nowrap（中文无词边界，不锁会断出半个词），
         * 于是最长那段是**不可压缩**的：36px 下它要 325px，而 320px 视口的容器只有
         * 280px —— 会被 Hero section 的 overflow-hidden 直接裁掉右半句。
         * 30px 下同一段是 271px，装得下。
         */
        "hero-xs": ["1.875rem", { lineHeight: "1.2", letterSpacing: "-0.02em" }],
        /**
         * 区块标题（`sm` 以上）。
         *
         * 原先是 `2rem` —— 只比 `text-3xl`（30px）大 2px，而且它自带
         * `letterSpacing: -0.01em`，**反向覆盖**了调用点显式写的 `tracking-tight`
         * （-0.025em），于是「响应式放大」实际上只放大 2px 还把字距放松了。
         * 现在 36px + -0.025em：既真的放大一档，也与 tracking-tight 同向。
         */
        "section-title": ["2.25rem", { lineHeight: "1.2", letterSpacing: "-0.025em" }],
      },
      keyframes: {
        // 进入视口的轻微淡入上移，位移刻意只有 12px —— 幅度再大就成了干扰动画
        "fade-up": {
          from: { opacity: "0", transform: "translateY(12px)" },
          to: { opacity: "1", transform: "translateY(0)" },
        },
        "fade-in": {
          from: { opacity: "0" },
          to: { opacity: "1" },
        },
        // FAQ 手风琴展开
        "accordion-down": {
          from: { height: "0" },
          to: { height: "var(--accordion-height)" },
        },
      },
      animation: {
        "fade-up": "fade-up 420ms cubic-bezier(0.16, 1, 0.3, 1) both",
        "fade-in": "fade-in 300ms ease-out both",
      },
      transitionDuration: {
        250: "250ms",
      },
    },
  },
  plugins: [],
};
