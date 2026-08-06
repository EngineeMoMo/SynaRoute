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
      },

      maxWidth: {
        // 正文主容器宽度：模板建议 1120~1280，取中间值兼顾大屏留白与信息密度
        content: "1200px",
        // 文档正文单列宽度，避免长行影响阅读
        prose: "72ch",
      },
      fontSize: {
        // Hero 主标题：桌面 56px / 移动 36px（由响应式类切换），行高压紧显紧凑
        hero: ["3.5rem", { lineHeight: "1.1", letterSpacing: "-0.02em" }],
        "hero-sm": ["2.25rem", { lineHeight: "1.15", letterSpacing: "-0.02em" }],
        "section-title": ["2rem", { lineHeight: "1.25", letterSpacing: "-0.01em" }],
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
