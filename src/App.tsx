import { useEffect, useState } from "react";
import { Sidebar, type NavKey } from "@/components/Sidebar";
import { CategoryPage } from "@/pages/CategoryPage";
import { BrainPage } from "@/pages/BrainPage";
import { LogsPage } from "@/pages/LogsPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { AboutPage } from "@/pages/AboutPage";
import { VendorPage } from "@/pages/VendorPage";
import { KeyEditor } from "@/components/KeyEditor";
import { Toast } from "@/components/Toast";
import { UpdateBanner } from "@/components/UpdateBanner";
import { ConfigAppliedDialog } from "@/components/ConfigAppliedDialog";
import { useStore, applyTheme } from "@/store";
import { isTauri } from "@/lib/bridge";
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
  const theme = useStore((s) => s.theme);
  const t = useT();
  const [nav, setNav] = useState<NavKey>("claude-cli");
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingKey, setEditingKey] = useState<ProviderKey | null>(null);

  // 初次加载
  useEffect(() => {
    void loadSettings();
    void loadVendors();
    void loadCategory(activeCategory);
    // 启动后静默检查更新（失败不弹窗，仅供顶部横幅 / 设置页展示）。
    // 不加 isTauri 守卫：浏览器预览下 bridge 会走 mock，正好能验证横幅布局；
    // 真实环境走 Tauri 命令。两边都不弹窗，失败仅落进 updateCheck.error。
    void useStore.getState().checkForUpdates({ silent: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
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

  const openAdd = () => {
    setEditingKey(null);
    setEditorOpen(true);
  };
  const openEdit = (k: ProviderKey) => {
    setEditingKey(k);
    setEditorOpen(true);
  };

  const renderMain = () => {
    if (nav === "brain") return <BrainPage />;
    if (nav === "logs") return <LogsPage />;
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
        <Sidebar active={nav} onSelect={handleNav} />

        <main className="flex-1 overflow-hidden">{renderMain()}</main>
      </div>

      {editorOpen && (
        <KeyEditor initial={editingKey} onClose={() => setEditorOpen(false)} />
      )}

      <Toast />
      <ConfigAppliedDialog />

      {/* 浏览器预览模式提示（Tauri 环境不显示） */}
      {!isTauri() && (
        <div className="pointer-events-none fixed bottom-3 left-1/2 -translate-x-1/2 rounded-full bg-warning/90 px-3 py-1 text-[11px] font-medium text-white shadow-lg">
          {t("app.browserPreview")}
        </div>
      )}
    </div>
  );
}
