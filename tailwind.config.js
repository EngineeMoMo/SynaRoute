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
        // 「内嵌块」底色（代码块、路径展示、只读信息面板）。5 处代码在用它，
        // 但 token 一直没注册 → 那 5 块**一条底色 CSS 都没生成**，看着是透明的，
        // 与外层面板糊在一起（`bg-warning/8` 同一类静默失效，只是原因从「透明度不在标度」
        // 换成「token 不存在」）。绑到 --surface-hover：它比 surface 略深一档，
        // 正是「内嵌/下沉」想要的对比，且深浅主题都已定义、不必新增变量。
        // 判据：`npm run build` 后 dist CSS 里能搜到 `.bg-surface-elevated`。
        "surface-elevated": "rgb(var(--surface-elevated) / <alpha-value>)",
        "surface-overlay": "rgb(var(--surface-overlay) / <alpha-value>)",
        border: "rgb(var(--border) / <alpha-value>)",
        "border-strong": "rgb(var(--border-strong) / <alpha-value>)",
        input: "rgb(var(--border) / <alpha-value>)",
        ring: "rgb(var(--primary) / <alpha-value>)",
        scrim: "rgb(var(--scrim) / <alpha-value>)",
        "text-primary": "rgb(var(--text-primary) / <alpha-value>)",
        "text-secondary": "rgb(var(--text-secondary) / <alpha-value>)",
        "text-muted": "rgb(var(--text-muted) / <alpha-value>)",
        primary: {
          DEFAULT: "rgb(var(--primary) / <alpha-value>)",
          // 渐变深端（UI-1）。注册成 primary.deep 后 `to-primary-deep` 才会生成 CSS——
          // 不注册就写这个类名，属于本仓踩过两次的「静默失效」：不报错、只是零 CSS。
          // 为什么是「深」而不是方案里写的「浅」：见 styles.css 里 --primary-deep 的注释
          //（往亮处渐变会让白字对比度跌破 WCAG AA）。
          deep: "rgb(var(--primary-deep) / <alpha-value>)",
          foreground: "rgb(var(--primary-foreground) / <alpha-value>)",
        },
        route: {
          DEFAULT: "rgb(var(--route) / <alpha-value>)",
          deep: "rgb(var(--route-deep) / <alpha-value>)",
          foreground: "rgb(var(--route-foreground) / <alpha-value>)",
        },
        success: "rgb(var(--success) / <alpha-value>)",
        warning: "rgb(var(--warning) / <alpha-value>)",
        danger: "rgb(var(--danger) / <alpha-value>)",
        info: "rgb(var(--info) / <alpha-value>)",
      },
      borderRadius: {
        card: "14px",
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
      /**
       * 海拔三档，**读 CSS 变量而不是在这里算**。
       *
       * 🔴 原先写的是 `rgb(var(--shadow-rgb) / 0.06)` 这种「一个颜色变量 + 低透明度」
       * 的公式。它在浅色下勉强可见，在深色下**等于一条 CSS 都没写** ——
       * `--shadow-rgb: 0 0 0` @ 0.06 压在 #0E0E11 上算出来差不到 1/255。
       * 官网踩过同一个坑，表现是深色模式下所有 `hover:shadow-card-hover` 毫无反馈。
       *
       * 公式驱动做不到「浅深两套不同 alpha」，所以改成值驱动：成品阴影定义在
       * src/styles.css 的 :root / .dark 各一组。类名不变，全部调用点自动升级。
       *
       * 判据：`npm run build` 后 dist CSS 里 `.shadow-card` 的值应是 `var(--shadow-card)`，
       * 且 styles.css 的 `:root` 与 `.dark` 里都能搜到该变量。
       */
      boxShadow: {
        card: "var(--shadow-card)",
        "card-hover": "var(--shadow-card-hover)",
        elevated: "var(--shadow-elevated)",
        dialog: "var(--shadow-dialog)",
      },
    },
  },
  plugins: [],
};
