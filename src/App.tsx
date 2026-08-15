import * as React from "react";
import { useEffect, useState } from "react";
import { Sidebar, type NavKey } from "@/components/Sidebar";
import { CategoryPage } from "@/pages/CategoryPage";
import { BrainPage } from "@/pages/BrainPage";
import { LogsPage } from "@/pages/LogsPage";
import { UsagePage } from "@/pages/UsagePage";
import { SettingsPage } from "@/pages/SettingsPage";
import { AboutPage } from "@/pages/AboutPage";
import { VendorPage } from "@/pages/VendorPage";
import { KeyEditor } from "@/components/KeyEditor";
import { CommandPalette } from "@/components/CommandPalette";
import { OnboardingWizard } from "@/components/OnboardingWizard";
import { Toast } from "@/components/Toast";
import { UpdateBanner } from "@/components/UpdateBanner";
import { ConfigAppliedDialog } from "@/components/ConfigAppliedDialog";
import { useStore, applyTheme } from "@/store";
import { useBackendEvents } from "@/lib/useBackendEvents";
import { isTauri } from "@/lib/bridge";
import { requestNotificationPermission } from "@/lib/notifications";
import { useT } from "@/lib/useT";
import type { CategoryType, ProviderKey } from "@/types";

const CATEGORY_KEYS: NavKey[] = ["claude-cli", "claude-desktop", "codex"];

export default function App() {
  // 细粒度订阅（勿改回整店解构）：App 是根组件，整店订阅会让**整棵树**在任何 store
  // 字段变化时重渲染——包括 LogsPage 每 2s 的 events 刷新。
  const activeCategory = useStore((s) => s.activeCategory);
  const setActiveCategory = useStore((s) => s.setActiveCategory);
  const loadCategory = useStore((s) => s.loadCategory);
  const loadSettings = useStore((s) => s.loadSettings);
  const loadVendors = useStore((s) => s.loadVendors);
  const refreshCategory = useStore((s) => s.refreshCategory);
  const onboarding = useStore((s) => s.onboarding);
  const loadOnboarding = useStore((s) => s.loadOnboarding);

  // 后端状态推送（UX#5）：代理启停、配置落盘、健康态翻转、托盘快切模型等
  // 会由后端主动推过来，界面即时跟随；各页轮询退成 30s 兜底。
  useBackendEvents();
  const theme = useStore((s) => s.theme);
  const t = useT();
  const [nav, setNav] = useState<NavKey>("claude-cli");
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingKey, setEditingKey] = useState<ProviderKey | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);

  // 初次加载
  useEffect(() => {
    void loadSettings();
    void loadVendors();
    void loadCategory(activeCategory);
    void loadOnboarding();
    // 启动后静默检查更新（失败不弹窗，仅供顶部横幅 / 设置页展示）。
    // 不加 isTauri 守卫：浏览器预览下 bridge 会走 mock，正好能验证横幅布局；
    // 真实环境走 Tauri 命令。两边都不弹窗，失败仅落进 updateCheck.error。
    void useStore.getState().checkForUpdates({ silent: true });
    // 系统通知权限（FR-028 代理健康告警）：应用内警告条不需要权限，但系统级弹窗需要。
    // 只在 Tauri 环境请求（浏览器 mock 没有通知插件）。启动时请求一次：
    // 已授权则静默通过；未决定则弹系统授权框；拒绝则应用内警告条仍工作、仅系统弹窗不弹。
    void requestNotificationPermission();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /**
   * 全局快捷键 Ctrl/Cmd+K 开关命令面板（UX#7）。
   *
   * 三个判据，每个都对应一类真实故障：
   * - **`e.isComposing` 必须先挡**：中文输入法组字期间的 keydown 属于输入法，
   *   抢走会打断组字。本项目的 Key 备注名、大脑提示词都是中文输入重灾区。
   * - **用 `e.code === "KeyK"` 打头**：它是物理键位、与键盘布局无关；`e.key` 在
   *   非 US 布局下可能不是 "k"。两者都判是为了兼容 code 缺失的老 WebView。
   * - **排除 `altKey`**：避免和 Ctrl+Alt+K 一类输入法/系统组合抢键。
   *
   * 刻意**不检测 activeElement 是不是输入框**：Ctrl+K 带修饰键，不存在裸字母冲突；
   * 加了反而会让用户在面板自己的搜索框里按 Ctrl+K 关不掉面板。
   */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      const isK = e.code === "KeyK" || e.key?.toLowerCase() === "k";
      if (isK && (e.ctrlKey || e.metaKey) && !e.altKey) {
        e.preventDefault();
        setPaletteOpen((v) => !v); // 再按一次关闭
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // 窗口重新聚焦/可见时立即刷新到磁盘最新（纵深防御）。
  // 场景：单实例锁二次唤起已有窗口、用户切回窗口、外部改了配置——避免继续展示
  // 「进程启动那一刻」的陈旧快照（如 Key 数与磁盘背离）。这是对 CategoryPage 5s
  // 轮询 + 后端 mtime 自愈的补充：把「最坏等 5s」提前到「切回即最新」。
  useEffect(() => {
    let last = 0;
    const refresh = () => {
      const now = Date.now();
      if (now - last < 800) return; // 轻防抖：focus 与 visibilitychange 可能同刻触发
      last = now;
      void refreshCategory();
    };
    const onVis = () => {
      if (document.visibilityState === "visible") refresh();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [refreshCategory]);

  // 跟随系统主题变化
  useEffect(() => {
    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => applyTheme("system");
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, [theme]);

  const handleNav = (key: NavKey) => {
    setNav(key);
    if (CATEGORY_KEYS.includes(key)) {
      setActiveCategory(key as CategoryType);
    }
  };

  // useCallback 包住（PERF-2）：这两个回调会一路传到 KeyCard 的 props 上，
  // 而 KeyCard 是 memo 组件 —— 每次 App 重渲染都造新函数的话，memo 的浅比较在
  // onEdit 这一项上就失败，N 张卡片照样全量重渲染。App 订阅了多个 store 字段
  // （theme/onboarding/activeCategory…），它的重渲染并不罕见，所以这里必须稳。
  // setState 函数本身是稳定的，故依赖数组为空。
  const openAdd = React.useCallback(() => {
    setEditingKey(null);
    setEditorOpen(true);
  }, []);
  const openEdit = React.useCallback((k: ProviderKey) => {
    setEditingKey(k);
    setEditorOpen(true);
  }, []);

  const renderMain = () => {
    if (nav === "brain") return <BrainPage />;
    if (nav === "logs") return <LogsPage />;
    if (nav === "usage") return <UsagePage />;
    if (nav === "vendors") return <VendorPage />;
    if (nav === "settings") return <SettingsPage />;
    if (nav === "about") return <AboutPage />;
    // onOpenLogs：分类页的「最近失败原因」横幅要能一键跳到运行日志页看详情
    // （与 UpdateBanner 的 onOpenSettings 同一模式——nav 是 App 的局部状态，靠回调上抛）。
    return <CategoryPage onAddKey={openAdd} onEditKey={openEdit} onOpenLogs={() => handleNav("logs")} />;
  };

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-text-primary">
      {/* 更新横幅：整宽置顶、跨所有页面。原先只有侧栏 Logo 右上角一个小角标，不显眼。 */}
      <UpdateBanner onOpenSettings={() => handleNav("settings")} />

      <div className="flex min-h-0 flex-1">
        <Sidebar active={nav} onSelect={handleNav} onOpenPalette={() => setPaletteOpen(true)} />

        <main className="flex-1 overflow-hidden">{renderMain()}</main>
      </div>

      {editorOpen && (
        // key 强制重挂载：命令面板造出了「编辑器已开着、再从面板挑另一条 Key」这条路径。
        // React 18 会把 setEditorOpen(false) 与 openEdit(k) 批处理成 editorOpen 恒为 true，
        // 同类型同位置的元素不会重新挂载，而 KeyEditor 的字段全是 useState(initial?.x)
        // 只在挂载时取一次 —— 结果是抽屉标题换了、表单里还是上一条 Key 的 baseUrl 与密钥态。
        <KeyEditor key={editingKey?.id ?? "new"} initial={editingKey} onClose={() => setEditorOpen(false)} />
      )}

      {paletteOpen && (
        <CommandPalette
          onClose={() => setPaletteOpen(false)}
          // 每个动作都先关掉编辑器：面板可以跨分类跳转，而开着的编辑器属于旧分类，
          // 留着它会让用户在 A 分类的页面上对着 B 分类的 Key 表单。
          onNavigate={(k) => {
            setEditorOpen(false);
            handleNav(k);
          }}
          onEditKey={(k) => {
            setEditorOpen(false);
            openEdit(k);
          }}
          onAddKey={() => {
            setEditorOpen(false);
            openAdd();
          }}
        />
      )}

      <Toast />
      <ConfigAppliedDialog />
      {/*
        本项目**不做任何形态的悬浮球**（历史上试过两种，都已删除）：

        1. 应用内右下角悬浮球（`QuickPanel`）—— 与桌面悬浮窗职责重合，且用户点名不要
           应用内的那种形态；
        2. 桌面独立悬浮窗（`FloatingWidget` + `floating.rs`，2026-08-15 删除）——
           真机实测其子 WebView 从未初始化，用户彻底看不见，而后端却打出「已显示」。
           详见 docs/14 第十八节。

        软件内需要快捷入口走命令面板（Ctrl/Cmd+K）。要再做桌面浮层需重新立项，
        并先解决 WebView 初始化问题 —— 不要只把 UI 加回来。
      */}

      {/* 首启向导（UX#1）：仅在「标记未完成 且 一条 Key 都没有」时显示，可跳过。
          必须传 handleNav 本体——它同时做 setNav 与 setActiveCategory，
          只传 setActiveCategory 会让侧栏高亮与向导选的客户端对不上。 */}
      {onboarding?.shouldShow && (
        <OnboardingWizard
          ccswitchAvailable={onboarding.ccswitchAvailable}
          onPickCategory={handleNav}
          onOpenLogs={() => handleNav("logs")}
        />
      )}

      {/* 浏览器预览模式提示（Tauri 环境不显示） */}
      {!isTauri() && (
        <div className="pointer-events-none fixed bottom-3 left-1/2 -translate-x-1/2 rounded-full bg-warning/90 px-3 py-1 text-[11px] font-medium text-white shadow-lg">
          {t("app.browserPreview")}
        </div>
      )}
    </div>
  );
}
