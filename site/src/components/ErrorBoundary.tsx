import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * 渲染异常的**爆炸半径限制器**。
 *
 * # 为什么官网需要它
 *
 * React 18 对未捕获的渲染异常会**卸载整个 root**。实测过一次真实后果：
 * `marked` 用了 `Array.prototype.at`（Safari 15.4+ 才有），iOS 15.0~15.3 的用户
 * 打开任意文档页就抛 `TypeError` → `#root` 变空、顶栏页脚一起消失、
 * **而且此后连首页也回不来**（root 已经空了，客户端路由切过去也没有东西再挂上）。
 * 用户看到一整片纯白，只能认为「这站挂了」。
 *
 * 那个具体病因已由 `legacyPolyfills.ts` 修掉。这里挡的是**下一个**：
 * 任何一条内容页的渲染异常都不该让整站消失，而「以后不会再有渲染异常」是不能假设的。
 *
 * # 为什么不用现成库
 *
 * `react-error-boundary` 要多一个依赖，而 class 组件的 `getDerivedStateFromError`
 * 本来就三行。官网的依赖越少越好（首屏体积直接影响移动端）。
 *
 * # 呈现原则
 *
 * 出错时**必须仍然能导航**：给一条回首页的普通 `<a>`（不走 React Router ——
 * 路由本身可能就是出错的那一环），以及一个刷新按钮。
 * 不显示堆栈：访客看不懂，而且堆栈里可能带路径信息。堆栈进 console 供报障时贴。
 */
interface Props {
  children: ReactNode;
  /** 出错时的兜底 UI。省略则用下面那套默认文案。 */
  fallback?: ReactNode;
  /** 出现在错误信息里的区块名，便于 console 里定位。 */
  label?: string;
}

interface State {
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // 只进 console：访客看不懂堆栈，而报障的人需要它。
    console.error(`[SynaRoute site] ${this.props.label ?? "render"} 渲染失败`, error, info);
  }

  render() {
    if (!this.state.error) return this.props.children;
    if (this.props.fallback) return this.props.fallback;

    // 文案刻意不进 i18n：i18n 本身也可能是出错的那一环（它被 useLang 依赖），
    // 而这一屏必须在「什么都坏了」的时候还能显示出来。故中英双语直接并排写死。
    return (
      <div className="mx-auto max-w-xl px-5 py-24 text-center">
        <h1 className="text-2xl font-bold text-text-primary">这个页面没能显示出来</h1>
        <p className="mt-3 text-sm leading-relaxed text-text-secondary">
          可能是浏览器版本较旧。请尝试刷新，或升级到较新版本的 Safari / Chrome / Edge。
          <br />
          {/* 英文那句用 secondary 而不是 muted：muted 在浅色下 2.56:1、深色下
              2.20:1，而这一屏的读者恰恰是「页面坏了正在找线索」的人。 */}
          <span className="text-text-secondary">
            This page failed to render — try refreshing, or use a newer browser.
          </span>
        </p>
        <div className="mt-8 flex items-center justify-center gap-3">
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="min-h-11 rounded-control bg-primary-solid px-5 text-sm font-medium text-primary-foreground"
          >
            刷新 / Refresh
          </button>
          {/* 刻意用原生 <a>：React Router 可能正是出错的那一环 */}
          <a
            href="/"
            className="min-h-11 rounded-control border border-border px-5 py-2.5 text-sm text-text-secondary"
          >
            返回首页 / Home
          </a>
        </div>
      </div>
    );
  }
}
