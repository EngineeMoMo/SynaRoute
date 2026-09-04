// 大脑聚合的「运行面板」：Phase1 出计划 → **Phase2a 出预览** → Phase2b 才落盘。
//
// # 为什么中间要插一次确认
//
// 用户在 Phase1 确认的是**计划文本**，而完整文件内容是 Phase2 才生成的。原实现里
// 「确认执行」一点就直接写盘 —— 他从未看到将写入的字节，也没有「将改动这 N 个文件」
// 的清单，而这是整套功能里唯一不可逆的动作。现在中间多一屏：先看清单，再写。
//
// # 为什么单独一个文件
//
// `BrainPage.tsx` 顶在棘轮上（余量 1 行），而这一屏要新增预览清单。同时这块逻辑
// 与那一页其余部分（配置编辑）本来就没有耦合，只共享 `category`。

import { useState } from "react";
import { api } from "@/lib/bridge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { useT } from "@/lib/useT";
import type { AggregatePreview, CategoryType, PlannedChange } from "@/types";
import { Play, CheckCircle, FileWarning } from "lucide-react";

/** 面板所处的阶段。三个「进行中」态各自独立 —— 它们的提示语和可点按钮都不同。 */
type Phase = "idle" | "planning" | "planned" | "previewing" | "preview" | "writing" | "done";

export function BrainRunPanel({ category }: { category: CategoryType }) {
  const t = useT();
  const [prompt, setPrompt] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [plan, setPlan] = useState("");
  // Phase1 定下的工作目录与开始时刻，后两个阶段原样回传。
  //
  // ⚠️ `?? ""` 不能省：`Plan.work_dir` 为 None 时被 serde skip，前端拿到 undefined，
  // 而后端把 undefined 当成「调用方没给」。空字符串是约定的「Phase1 确认无工作目录」
  // 哨兵，后端据此不写任何文件。
  const [planWorkDir, setPlanWorkDir] = useState<string | undefined>(undefined);
  // 0 = 没有基准 → 后端会跳过「文件在确认期间被改过吗」那道防线。只在 Phase1 成功后有值。
  const [planStartedMs, setPlanStartedMs] = useState(0);
  const [preview, setPreview] = useState<AggregatePreview | null>(null);
  const [result, setResult] = useState("");
  const [error, setError] = useState<string | null>(null);

  const fail = (e: unknown, back: Phase) => {
    setError(String((e as Error)?.message ?? e));
    setPhase(back);
  };

  const runPlan = async () => {
    if (!prompt.trim()) return;
    setPhase("planning");
    setError(null);
    setPlan("");
    setPreview(null);
    setResult("");
    try {
      const res = await api.runAggregatePlan(category, prompt);
      if (res.resultType === "plan") {
        setPlan(res.content);
        setPlanWorkDir(res.workDir ?? "");
        setPlanStartedMs(res.planStartedMs ?? 0);
        setPhase("planned");
      } else {
        setResult(res.content);
        setPhase("done");
      }
    } catch (e) {
      fail(e, "idle");
    }
  };

  /** Phase2a：决策者出完整文件内容 + 预览。**不写盘。** */
  const runPreview = async () => {
    setPhase("previewing");
    setError(null);
    try {
      const res = await api.runAggregatePreview(
        category,
        prompt,
        plan,
        planWorkDir,
        planStartedMs,
      );
      setPreview(res);
      setPhase("preview");
    } catch (e) {
      fail(e, "planned");
    }
  };

  /** Phase2b：按决策者原文落盘。原文原样回传，服务端重新解析（同一道判定）。 */
  const runWrite = async () => {
    if (!preview) return;
    setPhase("writing");
    setError(null);
    try {
      const res = await api.runAggregateWrite(
        category,
        preview.content,
        planWorkDir,
        planStartedMs,
      );
      setResult(res.content);
      setPhase("done");
    } catch (e) {
      fail(e, "preview");
    }
  };

  const reset = () => {
    setPhase("idle");
    setPlan("");
    setPlanWorkDir(undefined);
    setPlanStartedMs(0);
    setPreview(null);
    setResult("");
    setError(null);
  };

  const writable = preview?.changes.filter((c) => !c.rejected) ?? [];
  const rejected = preview?.changes.filter((c) => c.rejected) ?? [];

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("brain.runTitle")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <textarea
          className="w-full rounded-control border border-border bg-background px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:outline-none focus:ring-2 focus:ring-ring"
          rows={3}
          placeholder={t("brain.runPlaceholder")}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          disabled={phase !== "idle"}
        />

        <div className="flex flex-wrap items-center gap-2">
          {phase === "idle" && (
            <Button size="sm" onClick={() => void runPlan()} disabled={!prompt.trim()}>
              <Play size={14} /> {t("brain.runStart")}
            </Button>
          )}
          {phase === "planning" && (
            <span className="text-xs text-text-muted">{t("brain.runThinking")}</span>
          )}
          {phase === "planned" && (
            <>
              <Button size="sm" onClick={() => void runPreview()}>
                <CheckCircle size={14} /> {t("brain.runConfirm")}
              </Button>
              <Button size="sm" variant="outline" onClick={reset}>
                {t("common.cancel")}
              </Button>
            </>
          )}
          {phase === "previewing" && (
            <span className="text-xs text-text-muted">{t("brain.runPreviewing")}</span>
          )}
          {phase === "preview" && (
            <>
              {/* 无可写项时不给写入按钮 —— 点了什么都不会发生，那是个空承诺。 */}
              {writable.length > 0 && (
                <Button size="sm" onClick={() => void runWrite()}>
                  <CheckCircle size={14} />{" "}
                  {t("brain.runWrite").replace("{n}", String(writable.length))}
                </Button>
              )}
              <Button size="sm" variant="outline" onClick={reset}>
                {t("common.cancel")}
              </Button>
            </>
          )}
          {phase === "writing" && (
            <span className="text-xs text-text-muted">{t("brain.runWriting")}</span>
          )}
          {phase === "done" && (
            <Button size="sm" variant="secondary" onClick={reset}>
              {t("brain.runReset")}
            </Button>
          )}
        </div>

        {error && <div className="text-xs text-danger">{error}</div>}

        {plan && (
          <div>
            <div className="mb-1 text-xs font-medium text-text-secondary">
              {t("brain.runPlanTitle")}
            </div>
            <pre className="max-h-64 overflow-auto rounded-control border border-border bg-background p-3 font-mono text-xs leading-relaxed text-text-primary">
              {plan}
            </pre>
          </div>
        )}

        {preview && phase === "preview" && (
          <PreviewList
            preview={preview}
            writable={writable}
            rejected={rejected}
            noWorkDir={!planWorkDir}
          />
        )}

        {result && phase === "done" && (
          <div>
            <div className="mb-1 text-xs font-medium text-success">
              {t("brain.runResultTitle")}
            </div>
            <pre className="max-h-64 overflow-auto rounded-control border border-success/30 bg-success/5 p-3 font-mono text-xs leading-relaxed text-text-primary">
              {result}
            </pre>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** 落盘清单：一行一个文件，标出「覆盖 / 新建 / 已拒绝」与字节数。 */
function PreviewList({
  preview,
  writable,
  rejected,
  noWorkDir,
}: {
  preview: AggregatePreview;
  writable: PlannedChange[];
  rejected: PlannedChange[];
  noWorkDir: boolean;
}) {
  const t = useT();
  return (
    <div className="space-y-2">
      <div className="text-xs font-medium text-text-secondary">{t("brain.runPreviewTitle")}</div>
      {noWorkDir ? (
        <div className="rounded-control border border-warning/30 bg-warning/8 p-2 text-xs text-text-secondary">
          {t("brain.runNoWorkDir")}
        </div>
      ) : preview.changes.length === 0 ? (
        <div className="rounded-control border border-border bg-background p-2 text-xs text-text-secondary">
          {t("brain.runNoChanges")}
        </div>
      ) : (
        <>
          <div className="text-xs text-text-muted">{t("brain.runPreviewHint")}</div>
          {preview.workDir && (
            <div className="font-mono text-xs text-text-muted">{preview.workDir}</div>
          )}
          <ul className="space-y-1">
            {/* key 必须带序号：同一路径**可以**在清单里出现两次（决策者重复输出时，第二条
                会带「重复块」的拒绝原因），裸用 path 会撞 React key —— 同 BrainPage 里
                `updateMemberModel` 那条确定性 id 踩过的坑。 */}
            {preview.changes.map((c, i) => (
              <li
                key={`${i}-${c.path}`}
                className="flex flex-wrap items-center gap-2 rounded-control border border-border bg-background px-2 py-1.5 text-xs"
              >
                {c.rejected ? (
                  <Badge variant="danger">{t("brain.runRejectedTag")}</Badge>
                ) : c.exists ? (
                  <Badge variant="warning">{t("brain.runOverwrite")}</Badge>
                ) : (
                  <Badge variant="success">{t("brain.runCreate")}</Badge>
                )}
                <span className="font-mono text-text-primary">{c.path}</span>
                <span className="text-text-muted">{c.bytes} B</span>
                {c.rejected && (
                  <span className="flex items-center gap-1 text-text-secondary">
                    <FileWarning size={12} /> {c.rejected}
                  </span>
                )}
              </li>
            ))}
          </ul>
          {rejected.length > 0 && (
            <div className="text-xs text-warning">
              {t("brain.runRejectedCount").replace("{n}", String(rejected.length))}
            </div>
          )}
          {writable.some((c) => c.exists) && (
            <div className="text-xs text-text-muted">{t("brain.runBackupNote")}</div>
          )}
        </>
      )}
      <details>
        <summary className="cursor-pointer text-xs text-text-secondary">
          {t("brain.runDeciderOutput")}
        </summary>
        <pre className="mt-1 max-h-64 overflow-auto rounded-control border border-border bg-background p-3 font-mono text-xs leading-relaxed text-text-primary">
          {preview.content}
        </pre>
      </details>
    </div>
  );
}
