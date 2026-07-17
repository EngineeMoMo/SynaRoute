import { useEffect, useState } from "react";
import { Sidebar, type NavKey } from "@/components/Sidebar";
import { CategoryPage } from "@/pages/CategoryPage";
import { BrainPage } from "@/pages/BrainPage";
import { LogsPage } from "@/pages/LogsPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { VendorPage } from "@/pages/VendorPage";
import { KeyEditor } from "@/components/KeyEditor";
import { Toast } from "@/components/Toast";
import { ConfigAppliedDialog } from "@/components/ConfigAppliedDialog";
import { useStore, applyTheme } from "@/store";
import { isTauri } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { CategoryType, ProviderKey } from "@/types";

const CATEGORY_KEYS: NavKey[] = ["claude-cli", "claude-desktop", "codex"];

export default function App() {
  const { activeCategory, setActiveCategory, loadCategory, loadSettings, loadVendors, theme } = useStore();
  const t = useT();
  const [nav, setNav] = useState<NavKey>("claude-cli");
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingKey, setEditingKey] = useState<ProviderKey | null>(null);

  // 初次加载
  useEffect(() => {
    void loadSettings();
    void loadVendors();
    void loadCategory(activeCategory);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
    return <CategoryPage onAddKey={openAdd} onEditKey={openEdit} />;
  };

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-background text-text-primary">
      <Sidebar active={nav} onSelect={handleNav} />

      <main className="flex-1 overflow-hidden">{renderMain()}</main>

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
