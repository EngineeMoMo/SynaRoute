/** @type {import('tailwindcss').Config} */
export default {
  darkMode: ["class"],
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // 语义色 token —— 与 CSS 变量绑定，便于深浅主题切换（见 03-UIUX设计文档 §3.1）
        background: "rgb(var(--background) / <alpha-value>)",
        surface: "rgb(var(--surface) / <alpha-value>)",
        "surface-hover": "rgb(var(--surface-hover) / <alpha-value>)",
        border: "rgb(var(--border) / <alpha-value>)",
        input: "rgb(var(--border) / <alpha-value>)",
        ring: "rgb(var(--primary) / <alpha-value>)",
        "text-primary": "rgb(var(--text-primary) / <alpha-value>)",
        "text-secondary": "rgb(var(--text-secondary) / <alpha-value>)",
        "text-muted": "rgb(var(--text-muted) / <alpha-value>)",
        primary: {
          DEFAULT: "rgb(var(--primary) / <alpha-value>)",
          foreground: "rgb(var(--primary-foreground) / <alpha-value>)",
        },
        success: "rgb(var(--success) / <alpha-value>)",
        warning: "rgb(var(--warning) / <alpha-value>)",
        danger: "rgb(var(--danger) / <alpha-value>)",
        info: "rgb(var(--info) / <alpha-value>)",
      },
      borderRadius: {
        card: "16px",
        control: "10px",
        pill: "9999px",
      },
      // Tailwind 3.4 的 opacity 标度只有 5 的倍数，`bg-warning/8`、`bg-danger/12` 这类
      // 非 5 倍数的透明度修饰符**不会报错，只是一条 CSS 规则都不生成** —— 元素照常渲染，
      // 只是底色悄悄没了。全仓有 20 处这么写（各类警告横幅 + Badge 的全部彩色变体），
      // 一直表现为「有边框有字色、独独没有底色」，因为看着仍像个提示框而长期没被发现。
      // 在此把这两档补进标度，而不是把 20 处类名改成 /10 —— 保住设计稿原本的 8%/12%。
      // 判据：`npm run build` 后 `dist/assets/*.css` 里应能搜到 `.bg-warning\/8`。
      opacity: {
        8: "0.08",
        12: "0.12",
      },
      fontFamily: {
        sans: ['"Segoe UI"', '"Microsoft YaHei"', "system-ui", "sans-serif"],
        mono: ['"Cascadia Code"', "Consolas", "monospace"],
      },
      boxShadow: {
        card: "0 1px 2px rgba(0,0,0,.04)",
        "card-hover": "0 4px 12px rgba(0,0,0,.08)",
      },
    },
  },
  plugins: [],
};
