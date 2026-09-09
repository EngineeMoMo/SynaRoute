/**
 * codegraph 可用状态面板。从 `BrainPage.tsx` 抽出 —— 那个文件顶着棘轮冻结值，
 * 而本仓腾空间的正确动作是**删或搬**，不是重排。
 *
 * 组件自身的取舍与那条「目录一律由后端判定」的缺陷记录都在下面的函数文档里。
 */
import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { Activity, CheckCircle, Info, Wand2 } from "lucide-react";
import type { TFunc } from "@/lib/i18n";
import type { CategoryType, CodegraphState } from "@/types";
/**
 * codegraph 可用状态面板 —— 装了就让检索走「符号 + 调用链」精确切片，没装则退化为按文件检索。
 *
 * 刻意**不做自动安装**：往用户机器装第三方 CLI 属于要明确确认的动作，这里只给出命令让用户自己跑，
 * 装完点「重新检测」。建索引是本地操作（在项目里生成 .codegraph/），故提供一键按钮。
 *
 * 🔴 **目录一律由后端判定，本组件不算目录**（原实现在自动跟随时传 `undefined`，于是后端
 * 报「未索引」+ 禁用按钮 + 「请先设置工作目录」，而活跃项目就显示在同一屏上方 —— 用户报的
 * 缺陷本体）。`workDir`/`autoFollow` 只用于**触发重新检测**，不参与目录计算。
 * 理由全文见 `lib.rs::codegraph_dir`。
 */
export function CodegraphPanel({
  t,
  categoryId,
  workDir,
  autoFollow,
}: {
  t: TFunc;
  categoryId: CategoryType;
  workDir?: string;
  autoFollow: boolean;
}) {
  const [state, setState] = useState<CodegraphState | null>(null);
  const [checking, setChecking] = useState(false);
  const [building, setBuilding] = useState(false);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const check = async () => {
    setChecking(true);
    try {
      setState(await api.detectCodegraph(categoryId));
    } catch (e) {
      setMsg({ kind: "err", text: String((e as Error)?.message ?? e) });
    } finally {
      setChecking(false);
    }
  };

  // 目录设置变化时重新检测（换项目 / 切自动跟随后索引状态会变）。
  // 依赖里放这两个 prop 而不是它们算出的目录：目录在后端算，这里只需要知道「可能变了」。
  useEffect(() => {
    void check();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [categoryId, workDir, autoFollow]);

  const build = async () => {
    setBuilding(true);
    setMsg(null);
    try {
      const summary = await api.codegraphInit(categoryId);
      setMsg({ kind: "ok", text: summary });
      await check();
    } catch (e) {
      setMsg({ kind: "err", text: String((e as Error)?.message ?? e) });
    } finally {
      setBuilding(false);
    }
  };

  const s = state?.state;
  const desc =
    s === "ready"
      ? t("brain.cgDescReady")
      : s === "notIndexed"
        ? t("brain.cgDescNotIndexed")
        : s === "indexUnknown"
          ? t("brain.cgDescIndexUnknown")
          : s === "stranded"
            ? t("brain.cgDescStranded")
            : t("brain.cgDescNotInstalled");

  return (
    <div className="space-y-2 border-t border-border pt-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-text-primary">{t("brain.cgTitle")}</span>
            {s === "ready" && (
              <Badge variant="success">
                <CheckCircle size={10} /> {state && "version" in state ? `v${state.version}` : ""}
              </Badge>
            )}
            {s === "notIndexed" && <Badge variant="warning">{t("brain.cgBadgeNotIndexed")}</Badge>}
            {s === "indexUnknown" && <Badge variant="neutral">{t("brain.cgBadgeIndexUnknown")}</Badge>}
            {s === "stranded" && <Badge variant="warning">PATH</Badge>}
            {s === "notInstalled" && <Badge variant="neutral">{t("brain.cgBadgeNotInstalled")}</Badge>}
          </div>
          <div className="mt-1 text-xs text-text-muted">{desc}</div>
          {/* 判定到的目录必须显示：自动跟随下它来自会话历史，用户看到「未索引」时第一个问题是「哪个项目」 */}
          {state && "dir" in state && (
            <div className="mt-1 truncate font-mono text-[11px] text-text-muted" title={state.dir}>
              {state.dir}
            </div>
          )}
        </div>
        <Button size="sm" variant="secondary" onClick={() => void check()} disabled={checking}>
          <Activity size={13} /> {t("brain.cgRecheck")}
        </Button>
      </div>

      {/* 未安装 / 孤岛：给命令让用户自己装，不代劳 */}
      {(s === "notInstalled" || s === "stranded") && (
        <div className="space-y-1">
          <div className="text-xs text-text-secondary">{t("brain.cgInstallHint")}</div>
          <code className="block select-all rounded-control border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-text-primary">
            npm i -g @colbymchenry/codegraph
          </code>
        </div>
      )}

      {/* 已安装且**目录已判定**但项目未索引：一键建索引（纯本地）。后端已给出 dir，按钮永不禁用 */}
      {s === "notIndexed" && (
        <Button size="sm" onClick={() => void build()} disabled={building}>
          <Wand2 size={13} /> {building ? t("brain.cgBuilding") : t("brain.cgBuildIndex")}
        </Button>
      )}

      {/* 只有真的判不出目录时才提示去设置 —— 这句话在别的状态下都是把用户送去做无效操作 */}
      {s === "indexUnknown" && (
        <span className="text-xs text-warning">{t("brain.cgNeedWorkDir")}</span>
      )}

      {msg && (
        <div className={`text-xs ${msg.kind === "ok" ? "text-success" : "text-danger"}`}>
          {msg.text}
        </div>
      )}

      <div className="flex items-center gap-1.5 text-xs text-text-muted">
        <Info size={12} /> {t("brain.cgLocalNote")}
      </div>
    </div>
  );
}
