import { useState } from "react";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { api } from "@/lib/bridge";
import { ArrowUpCircle, RefreshCw, X } from "lucide-react";

/**
 * 顶部全宽「发现新版本」横幅。
 *
 * 为什么改成横幅：更新提示原先只挂在侧栏 Logo 右上角一个 16px 角标上——与 Logo 图形挤在
 * 一起、又不在视线焦点路径上，实测用户看不见。横幅占满整宽、位于窗口最顶部，是桌面应用里
 * 更新提示的常规位置，且**在所有页面都显示**（原来的角标虽然也常驻，但视觉权重太低）。
 *
 * 关掉后本次启动内不再出现，但侧栏「设置」项的徽章仍在——提示可以关，入口不能丢。
 * 刻意不做持久化「永久忽略」：下次启动重新检查到就再提醒一次，避免用户一次误点后
 * 再也不知道有新版本。
 */
export function UpdateBanner({ onOpenSettings }: { onOpenSettings: () => void }) {
  const updateCheck = useStore((s) => s.updateCheck);
  const showToast = useStore((s) => s.showToast);
  const t = useT();
  const [dismissedVer, setDismissedVer] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);

  const ver = updateCheck?.status === "available" ? (updateCheck.version ?? null) : null;
  if (!ver || dismissedVer === ver) return null;

  const install = async () => {
    setInstalling(true);
    try {
      const msg = await api.installUpdate();
      showToast("success", msg);
      // 成功路径**刻意不复位 installing**：插件随后会重启应用，按钮保持禁用
      // 以免用户在重启窗口期内重复点击触发第二次下载。
    } catch (e) {
      // 安装失败必须可见：静默吞掉会让用户以为正在更新、然后一直等
      showToast(
        "error",
        t("settings.updateInstallError", { err: String((e as Error)?.message ?? e) }),
      );
      setInstalling(false);
    }
  };

  return (
    <div className="flex shrink-0 items-center gap-3 bg-primary px-4 py-2 text-primary-foreground shadow-sm">
      <ArrowUpCircle size={17} className="shrink-0" />

      {/* 文案整块可点：跳设置页看更新说明。比单独一个「详情」链接更好点中。 */}
      <button
        onClick={onOpenSettings}
        className="min-w-0 flex-1 truncate text-left text-sm hover:underline"
        title={t("update.viewNotes")}
      >
        <span className="font-semibold">{t("update.bannerTitle", { version: ver })}</span>
        <span className="ml-2 text-xs opacity-80">{t("update.viewNotes")}</span>
      </button>

      <button
        onClick={() => void install()}
        disabled={installing}
        className="shrink-0 rounded-control bg-white/95 px-3 py-1 text-xs font-semibold text-primary transition-colors hover:bg-white disabled:opacity-60"
      >
        {installing ? (
          <span className="flex items-center gap-1.5">
            <RefreshCw size={12} className="animate-spin" />
            {t("update.installing")}
          </span>
        ) : (
          t("update.installNow")
        )}
      </button>

      <button
        onClick={() => setDismissedVer(ver)}
        className="shrink-0 rounded p-1 opacity-80 transition-colors hover:bg-white/15 hover:opacity-100"
        title={t("update.later")}
        aria-label={t("update.later")}
      >
        <X size={14} />
      </button>
    </div>
  );
}
