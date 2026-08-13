/**
 * 桌面悬浮球（第⑥批，2026-08-13 由矩形卡片改为球）
 *
 * 纯前端实现的单页 WebView 组件。后端在 `set_floating_widget(true)` 时创建、
 * 用户关闭或 `false` 时销毁。
 *
 * ## 形态：常态一颗球，悬停展开明细
 *
 * 原先是 300×208 的矩形卡片，四行明细常驻铺开 —— 在桌面上很挡事，而它想说的话
 * 只在用户主动看一眼时才需要。现在收成 64×64 的圆球（图标 + 运行数），
 * 鼠标移上去才展开成原来那张面板。
 *
 * **圆形靠三件事同时成立**，少一件都还是方的：
 * 1. 窗口 `transparent(true)` —— 否则四角露出方形底色；
 * 2. 窗口 `shadow(false)` —— 系统阴影按**窗口矩形**投，留着就投出一个方影子；
 * 3. 窗口宽高相等 —— 不等的话 `rounded-full` 画出来是椭圆。
 * 前两条在 `floating.rs` 的 builder 里，第三条是 `FLOATING_BALL` 常量。
 *
 * **展开必须同时改窗口尺寸**（`api.setFloatingExpanded`）：WebView 画不出窗口以外的
 * 东西，光在 CSS 里展开会被窗口边界直接裁掉，表现为「悬停没反应」。
 *
 * ## 注意事项
 * - 置顶不再写死，由设置项 `floatingWidgetAlwaysOnTop` 决定（默认关）——
 *   写死置顶就是「用别的软件时一直被它挡着」的原因。
 * - 组件需极致轻量：不加载完整路由、不依赖大型状态树。
 */

import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { ProxyState, CategoryType, UsageCostRow } from "@/types";

const CATS: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];

/** 某分类的初始占位状态（后端未答复前显示为 stopped，而不是空白）。 */
const idle = (categoryId: CategoryType): ProxyState => ({
  categoryId,
  status: "stopped",
  port: null,
});

export function FloatingWidget() {
  const t = useT();
  const [states, setStates] = useState<Record<CategoryType, ProxyState>>({
    "claude-cli": idle("claude-cli"),
    "claude-desktop": idle("claude-desktop"),
    codex: idle("codex"),
  });
  const [rows, setRows] = useState<UsageCostRow[]>([]);
  const [todayTokens, setTodayTokens] = useState(0);
  const [expanded, setExpanded] = useState(false);

  /**
   * 收起用防抖：鼠标从球移到展开面板的途中会短暂离开元素，
   * 不防抖就会「刚展开立刻收起」来回抖。移入时立即取消待执行的收起。
   */
  const collapseTimer = useRef<ReturnType<typeof setTimeout>>();

  const expand = () => {
    clearTimeout(collapseTimer.current);
    if (expanded) return;
    setExpanded(true);
    // 先改窗口再展开内容：窗口没变大就展开，内容会被窗口边界裁掉。
    void api.setFloatingExpanded(true);
  };

  const collapse = () => {
    clearTimeout(collapseTimer.current);
    collapseTimer.current = setTimeout(() => {
      setExpanded(false);
      void api.setFloatingExpanded(false);
    }, 260);
  };

  // 卸载时清掉待执行的收起，避免它在组件没了之后再打一次 IPC
  useEffect(() => () => clearTimeout(collapseTimer.current), []);

  useEffect(() => {
    let mounted = true;

    const fetchAll = async () => {
      try {
        const [cli, desktop, codex, cost, daily] = await Promise.all([
          api.getProxyState("claude-cli"),
          api.getProxyState("claude-desktop"),
          api.getProxyState("codex"),
          api.getUsageWithCost(),
          api.getDailyUsage(),
        ]);
        if (!mounted) return;
        setStates({ "claude-cli": cli, "claude-desktop": desktop, codex });
        setRows(cost);
        // 今日 token 按 **UTC** 日期取桶（与后端分桶同口径）。
        // 用本地时区会与后端错位一段，表现为跨零点前后数字忽然跳变。
        const today = new Date().toISOString().slice(0, 10);
        const bucket = daily.find((b) => b.date === today);
        setTodayTokens(
          (bucket?.entries ?? []).reduce(
            (s, e) =>
              s +
              e.usage.input +
              e.usage.output +
              (e.usage.cacheRead ?? 0) +
              (e.usage.cacheCreation ?? 0),
            0,
          ),
        );
      } catch (e) {
        // 悬浮窗读不到数据不弹错：它是个瞥一眼的小窗，一次 IPC 抖动
        // 不该让它变成报错框。保留上一次的显示。
        console.error("FloatingWidget refresh failed", e);
      }
    };

    void fetchAll();
    const refreshTimer = setInterval(fetchAll, 10_000);

    return () => {
      mounted = false;
      clearInterval(refreshTimer);
    };
  }, []);

  const runningCount = Object.values(states).filter((s) => s.status === "running").length;
  const costNano = rows.reduce((s, r) => s + (r.costNano ?? 0), 0);
  // 有 Key 算不出金额时标星号，避免用户把这个数当成全部花费。
  const anyUnpriced = rows.some((r) => r.costNano == null);

  // ---- 收起态：一颗球 ----
  //
  // 外层**必须透明且不占满**：窗口是方的（虽然设了 transparent），
  // 若外层给背景色就会露出方形。故背景只画在那颗 `rounded-full` 的球上。
  if (!expanded) {
    return (
      <div
        className="flex h-screen w-full items-center justify-center bg-transparent"
        onMouseEnter={expand}
      >
        {/* 球本体。`data-tauri-drag-region` 让整颗球都能拖 ——
            无边框窗没有标题栏，少了它用户就没法挪动。 */}
        <div
          data-tauri-drag-region
          className="relative flex h-14 w-14 cursor-move select-none items-center justify-center rounded-full border border-border bg-surface/95 shadow-lg backdrop-blur"
        >
          {/* 图标与角标都 pointer-events-none：让鼠标事件落到下面的拖动层，
              否则拖到图标上就拖不动了。 */}
          <img
            src="/app-icon.png"
            alt="SynaRoute"
            className="pointer-events-none h-6 w-6 rounded"
          />
          {/* 运行数角标：这是收起态唯一的信息，故一定要能一眼看清。
              0 个在跑时用灰色，不用绿色 —— 绿点会被读成「正常运行中」。 */}
          <span
            className={`pointer-events-none absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full px-1 text-[10px] font-semibold tabular-nums ${
              runningCount > 0
                ? "bg-success text-white"
                : "bg-surface-hover text-text-muted ring-1 ring-border"
            }`}
          >
            {runningCount}
          </span>
        </div>
      </div>
    );
  }

  // ---- 展开态：原来那张明细面板 ----
  return (
    // 用主题语义色而非硬编码深色：主窗口支持浅/深色主题，
    // 悬浮窗写死深色会在浅色主题下显得像另一个程序的窗口。
    <div
      className="relative h-screen w-full select-none overflow-hidden rounded-xl border border-border bg-surface/95 backdrop-blur"
      onMouseEnter={expand}
      onMouseLeave={collapse}
    >
      {/* 可拖动区域（铺满整窗）。无边框窗没有标题栏，
          少了 data-tauri-drag-region 用户就没法挪动它。 */}
      <div data-tauri-drag-region className="absolute inset-0 cursor-move" />

      {/* 内容层。pointer-events-none 让点击落到下面的拖动层上：
          悬浮窗整块都可拖，且不吃掉点击。 */}
      <div className="pointer-events-none relative z-10 flex h-full flex-col gap-2 p-3">
        {/* 标题行：用**软件自己的图标**（用户明确要求），不是通用闪电图标 */}
        <div className="flex items-center gap-2">
          <img src="/app-icon.png" alt="SynaRoute" className="h-4 w-4 shrink-0 rounded" />
          <span className="flex-1 truncate text-xs font-semibold text-text-primary">
            SynaRoute
          </span>
          <span
            className={`shrink-0 rounded-full px-1.5 py-0.5 text-[10px] ${
              runningCount > 0 ? "bg-success/12 text-success" : "bg-surface-hover text-text-muted"
            }`}
          >
            {t("floating.runningCount", { n: runningCount })}
          </span>
        </div>

        {/* 三端代理状态：点 + 名字 + 端口，一行一个 */}
        <div className="space-y-1">
          {CATS.map((cat) => {
            const st = states[cat];
            const running = st.status === "running";
            return (
              <div key={cat} className="flex items-center gap-1.5 text-[11px]">
                <span
                  className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                    running ? "bg-success" : "bg-text-muted"
                  }`}
                />
                <span className="flex-1 truncate text-text-secondary">{t(`nav.${cat}`)}</span>
                {running && st.port && (
                  <span className="shrink-0 font-mono text-text-muted">:{st.port}</span>
                )}
              </div>
            );
          })}
        </div>

        {/* 今日 token 与累计花费估算 */}
        <div className="mt-auto grid grid-cols-2 gap-2 border-t border-border pt-2">
          <div>
            <div className="text-[10px] text-text-muted">{t("floating.todayTokens")}</div>
            <div className="font-mono text-xs tabular-nums text-text-primary">
              {fmtTokens(todayTokens)}
            </div>
          </div>
          <div>
            <div className="text-[10px] text-text-muted">{t("floating.estCost")}</div>
            <div className="font-mono text-xs tabular-nums text-primary">
              {fmtUsd(costNano)}
              {anyUnpriced && <span className="ml-0.5 text-warning">*</span>}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/** 紧凑 token 数（与用量页 fmt 同口径，便于两处对照）。 */
function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 10_000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** 纳美元 → 美元，4 位小数（与后端 format_usd_from_nano 同口径）。 */
function fmtUsd(nano: number): string {
  const d = nano / 1_000_000_000;
  if (d > 0 && d < 0.0001) return "<$0.0001";
  return `$${d.toFixed(4)}`;
}

