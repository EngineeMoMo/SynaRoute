import * as React from "react";

/**
 * 全局渲染错误边界。
 *
 * ## 为什么必须有
 *
 * React 里**任何一个组件 render 抛异常，整棵树都会被卸载** —— 用户看到的是一整片空白窗口，
 * 没有任何文字、没有报错、连侧栏都没了。而这个应用此前**一处 ErrorBoundary 都没有**：
 * 真机反馈「用量统计整页空白」正是这个形态（一条脏数据让某个 `.map()` 抛错 →
 * 全窗口白屏 → 用户以为程序坏了，且无从提供任何线索）。
 *
 * 白屏是最坏的失败形态：它同时毁掉「用户能自救」（看不到是哪里错）和
 * 「开发能定位」（拿不到栈）两件事。有了边界后，异常被收敛成一张可读的错误卡片，
 * 并且**其余部分照常工作**（边界套在页面容器内，侧栏与导航不受影响，用户能切走）。
 *
 * ## 刻意的行为
 *
 * - **显示原始错误消息与栈**，不做美化：用户会把这段截图发过来，那是唯一的线索来源。
 * - 提供「重试」：清掉错误状态重新挂载子树。多数渲染异常源于一次性的脏数据，
 *   重新拉一次数据就好了，不必让用户重启整个应用。
 * - `key` 变化时自动复位：切换页面时不该继续显示上一页的错误。
 * - 同时 `console.error`：便于在 devtools / 日志里留痕。
 */
interface Props {
  children: React.ReactNode;
  /** 出错时显示的区域名（如「用量统计」），让用户知道是哪一块坏了 */
  label?: string;
  /** 变化时自动清除错误（通常传当前页面 id） */
  resetKey?: string;
}

interface State {
  error: Error | null;
  stack: string;
}

export class ErrorBoundary extends React.Component<Props, State> {
  state: State = { error: null, stack: "" };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    // 留痕：devtools 控制台 + 组件栈。用户反馈时这两段是主要判据。
    console.error("[ErrorBoundary] 渲染异常:", error, info.componentStack);
    this.setState({ stack: info.componentStack ?? "" });
  }

  componentDidUpdate(prev: Props) {
    // 切页面即复位，避免把上一页的错误带过来。
    if (prev.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null, stack: "" });
    }
  }

  render() {
    const { error, stack } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8">
        <div className="w-full max-w-2xl rounded-card border border-danger/40 bg-danger/8 p-5">
          <div className="text-sm font-semibold text-danger">
            {this.props.label ? `「${this.props.label}」渲染出错` : "页面渲染出错"}
          </div>
          <p className="mt-1 text-xs leading-relaxed text-text-secondary">
            这一块内容没能显示出来，其余功能不受影响（可从左侧切换到别的页面）。
            把下面的信息截图反馈即可定位问题。
          </p>
          <pre className="mt-3 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-control bg-background p-2.5 font-mono text-[11px] text-text-secondary">
            {error.message || String(error)}
            {stack ? `\n${stack}` : ""}
          </pre>
          <button
            onClick={() => this.setState({ error: null, stack: "" })}
            className="mt-3 rounded-control border border-border px-3 py-1.5 text-xs text-text-secondary hover:bg-surface-hover"
          >
            重试
          </button>
        </div>
      </div>
    );
  }
}
