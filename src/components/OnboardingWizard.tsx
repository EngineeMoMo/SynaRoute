import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  Terminal,
  MonitorSmartphone,
  Code2,
  Check,
  KeyRound,
  Download,
  Play,
  Loader2,
  AlertTriangle,
  ArrowRight,
  Waypoints,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { CategoryType, FirstRequestProbe } from "@/types";
import type { NavKey } from "@/components/Sidebar";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { KeyEditor } from "@/components/KeyEditor";
import { CcSwitchImportDialog } from "@/components/CcSwitchImportDialog";

const CLIENTS: { cat: CategoryType; icon: LucideIcon }[] = [
  { cat: "claude-cli", icon: Terminal },
  { cat: "claude-desktop", icon: MonitorSmartphone },
  { cat: "codex", icon: Code2 },
];

interface Props {
  ccswitchAvailable: boolean;
  /** 必须传 App 的 handleNav 本体：它同时做 setNav 与 setActiveCategory */
  onPickCategory: (k: NavKey) => void;
  onOpenLogs: () => void;
}

/**
 * 首启四步向导（UX#1 / UX#2）。
 *
 * 解决的问题：新用户打开应用看到的是一个空分类页，得自己想明白
 * 「加 Key → 起代理 → 接入客户端 → 验证」这条链路，而其中任何一步做错都只表现为
 * 「用不了」，没有任何指引。
 *
 * **第④步是这里最关键的一步**：在此之前，用户接入完客户端后没有任何东西告诉他「成功了」，
 * 只能自己去开一个会话试。这一步靠「自向导开始起该分类有没有 route 事件」实时打勾，
 * 并且在收到失败时把失败原因直接摆出来 —— 只说「还没收到请求」帮不了配错的人。
 *
 * 复用而非重写：Key 用 KeyEditor、导入用 CcSwitchImportDialog、启动用 store.startProxy
 * （它内部 start + applyToolConfig，语义与界面按钮和托盘完全一致）。
 */
export function OnboardingWizard({ ccswitchAvailable, onPickCategory, onOpenLogs }: Props) {
  const t = useT();
  const dismissOnboarding = useStore((s) => s.dismissOnboarding);
  const startProxy = useStore((s) => s.startProxy);
  const keys = useStore((s) => s.keys);
  const proxy = useStore((s) => s.proxy);

  const [step, setStep] = useState(1);
  const [cat, setCat] = useState<CategoryType | null>(null);
  const [editorOpen, setEditorOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [probe, setProbe] = useState<FirstRequestProbe | null>(null);

  /**
   * 第④步的时间基准：**进入该步的那一刻**。
   *
   * 必须是「进入第④步」而不是「向导打开」——用户可能在第②步试过连通性、留下了
   * route 事件，那样一进第④步就直接打勾，用户根本没接入客户端却被告知成功了。
   */
  const sinceRef = useRef<number | null>(null);

  const hasKey = keys.length > 0;
  const running = proxy?.status === "running";

  // 第④步轮询探针。只在该步激活时跑，离开即停。
  useEffect(() => {
    if (step !== 4 || !cat) return;
    if (sinceRef.current === null) sinceRef.current = Date.now();
    const since = sinceRef.current;
    let alive = true;
    const tick = () => {
      void api
        .firstRequestSince(cat, since)
        .then((p) => {
          if (alive) setProbe(p);
        })
        .catch(() => {
          // 探针读不到就当作「还没收到」，不弹错误：这一步本来就是等待态，
          // 一次 IPC 抖动不该变成一条吓人的报错。
        });
    };
    tick();
    const id = setInterval(tick, 2000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [step, cat]);

  const pickClient = (c: CategoryType) => {
    setCat(c);
    onPickCategory(c as NavKey); // 同时切侧栏高亮与 activeCategory，否则后续操作作用在别的分类上
    setStep(2);
  };

  const doStart = async () => {
    setStarting(true);
    setStartError(null);
    // announce: false —— 本步骤自己就在说明写了什么配置，再弹一个「已写入」对话框会盖住向导
    const ok = await startProxy({ announce: false });
    setStarting(false);
    if (ok) setStep(4);
    else setStartError(t("onboarding.startFailed"));
  };

  const finish = () => void dismissOnboarding();

  const stepTitles = [
    t("onboarding.step1"),
    t("onboarding.step2"),
    t("onboarding.step3"),
    t("onboarding.step4"),
  ];

  return createPortal(
    <>
      {/* z-[80]：低于命令面板 z-[90] 与 Toast z-[100]，高于普通弹窗 z-50。
          向导会自己打开 KeyEditor（z-50），必须盖在向导之上才能填表 —— 所以向导不能比它高。
          这里用 80 是因为向导本身要盖住主界面，而 KeyEditor 走的是 portal 的后挂载顺序。 */}
      <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50 p-4">
        <div className="flex max-h-[86vh] w-[min(560px,94vw)] flex-col overflow-hidden rounded-lg border border-border bg-surface shadow-2xl">
          {/* 头部：进度指示 */}
          <div className="border-b border-border px-5 py-4">
            <div className="flex items-center gap-2">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-control bg-primary text-primary-foreground">
                <Waypoints size={17} />
              </div>
              <div className="min-w-0 flex-1">
                <div className="text-sm font-semibold text-text-primary">{t("onboarding.title")}</div>
                <div className="truncate text-[11px] text-text-muted">{t("onboarding.subtitle")}</div>
              </div>
              <button
                onClick={finish}
                className="shrink-0 text-xs text-text-muted underline underline-offset-2 hover:text-text-secondary"
              >
                {t("onboarding.skip")}
              </button>
            </div>
            <div className="mt-3 flex items-center gap-1.5">
              {stepTitles.map((title, i) => {
                const n = i + 1;
                const done = step > n;
                const active = step === n;
                return (
                  <div key={n} className="flex min-w-0 flex-1 items-center gap-1.5">
                    <span
                      className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[10px] font-medium ${
                        done
                          ? "bg-success text-white"
                          : active
                            ? "bg-primary text-primary-foreground"
                            : "bg-surface-hover text-text-muted"
                      }`}
                    >
                      {done ? <Check size={11} /> : n}
                    </span>
                    <span
                      className={`hidden truncate text-[11px] sm:block ${
                        active ? "font-medium text-text-primary" : "text-text-muted"
                      }`}
                    >
                      {title}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
            {/* ── 第①步：选客户端 ── */}
            {step === 1 && (
              <div className="space-y-2">
                <p className="text-xs leading-relaxed text-text-secondary">{t("onboarding.s1Desc")}</p>
                {CLIENTS.map(({ cat: c, icon: Icon }) => (
                  <button
                    key={c}
                    onClick={() => pickClient(c)}
                    className="flex w-full items-center gap-3 rounded-control border border-border px-3.5 py-3 text-left transition-colors hover:border-primary hover:bg-primary/8"
                  >
                    <Icon size={18} className="shrink-0 text-text-secondary" />
                    <span className="flex-1 text-sm text-text-primary">{t(`nav.${c}`)}</span>
                    <ArrowRight size={15} className="shrink-0 text-text-muted" />
                  </button>
                ))}
              </div>
            )}

            {/* ── 第②步：加 Key（手工 与 从 cc-switch 导入 并列为主选项）── */}
            {step === 2 && (
              <div className="space-y-3">
                <p className="text-xs leading-relaxed text-text-secondary">{t("onboarding.s2Desc")}</p>
                <div className="grid gap-2 sm:grid-cols-2">
                  {/*
                    两个选项**并列**、样式同级。检测到 cc-switch 库时把它排在前面并高亮：
                    对已经在用 cc-switch 的用户，导入比手工填十几个字段快得多；
                    没装的人则根本看不到这个选项，不会被一个点了会报错的按钮困惑。
                  */}
                  {ccswitchAvailable && (
                    <button
                      onClick={() => setImportOpen(true)}
                      className="flex flex-col items-start gap-1.5 rounded-control border-2 border-primary bg-primary/8 px-3.5 py-3 text-left transition-colors hover:bg-primary/12"
                    >
                      <Download size={17} className="text-primary" />
                      <span className="text-sm font-medium text-text-primary">
                        {t("onboarding.s2Import")}
                      </span>
                      <span className="text-[11px] leading-relaxed text-text-muted">
                        {t("onboarding.s2ImportHint")}
                      </span>
                    </button>
                  )}
                  <button
                    onClick={() => setEditorOpen(true)}
                    className="flex flex-col items-start gap-1.5 rounded-control border border-border px-3.5 py-3 text-left transition-colors hover:border-primary hover:bg-surface-hover"
                  >
                    <KeyRound size={17} className="text-text-secondary" />
                    <span className="text-sm font-medium text-text-primary">
                      {t("onboarding.s2Manual")}
                    </span>
                    <span className="text-[11px] leading-relaxed text-text-muted">
                      {t("onboarding.s2ManualHint")}
                    </span>
                  </button>
                </div>
                {hasKey && (
                  <div className="flex items-center gap-2 rounded-control border border-success/30 bg-success/8 px-3 py-2 text-xs text-success">
                    <Check size={14} className="shrink-0" />
                    <span className="flex-1">{t("onboarding.s2Done", { n: keys.length })}</span>
                  </div>
                )}
              </div>
            )}

            {/* ── 第③步：一键启动并接入 ── */}
            {step === 3 && (
              <div className="space-y-3">
                <p className="text-xs leading-relaxed text-text-secondary">{t("onboarding.s3Desc")}</p>
                <div className="rounded-control border border-border bg-surface-hover px-3.5 py-3 text-[11px] leading-relaxed text-text-secondary">
                  {t("onboarding.s3What")}
                </div>
                {startError && (
                  <div className="flex items-start gap-2 rounded-control border border-danger/30 bg-danger/8 px-3 py-2 text-xs text-danger">
                    <AlertTriangle size={14} className="mt-0.5 shrink-0" />
                    <span className="flex-1 leading-relaxed">{startError}</span>
                  </div>
                )}
                {running && !startError && (
                  <div className="flex items-center gap-2 rounded-control border border-success/30 bg-success/8 px-3 py-2 text-xs text-success">
                    <Check size={14} className="shrink-0" />
                    <span className="flex-1">{t("onboarding.s3Running")}</span>
                  </div>
                )}
              </div>
            )}

            {/* ── 第④步：验证真的收到了第一个请求 ── */}
            {step === 4 && (
              <div className="space-y-3">
                <p className="text-xs leading-relaxed text-text-secondary">
                  {t("onboarding.s4Desc", { cat: cat ? t(`nav.${cat}`) : "" })}
                </p>

                {probe?.routed ? (
                  <div className="flex items-start gap-2 rounded-control border border-success/30 bg-success/8 px-3 py-3 text-xs text-success">
                    <Check size={15} className="mt-0.5 shrink-0" />
                    <div className="flex-1 leading-relaxed">
                      <div className="font-medium">{t("onboarding.s4Got")}</div>
                      {probe.detail && <div className="mt-0.5 break-all opacity-80">{probe.detail}</div>}
                    </div>
                  </div>
                ) : (
                  <div className="flex items-center gap-2 rounded-control border border-border bg-surface-hover px-3 py-3 text-xs text-text-secondary">
                    <Loader2 size={15} className="shrink-0 animate-spin" />
                    <span className="flex-1 leading-relaxed">{t("onboarding.s4Waiting")}</span>
                  </div>
                )}

                {/* 收到了请求但失败：这时候只说「还没收到」是帮倒忙，得让他看见真实原因 */}
                {probe?.failed && !probe.routed && (
                  <div className="flex items-start gap-2 rounded-control border border-danger/30 bg-danger/8 px-3 py-2 text-xs text-danger">
                    <AlertTriangle size={14} className="mt-0.5 shrink-0" />
                    <div className="flex-1 leading-relaxed">
                      <div className="font-medium">{t("onboarding.s4Failed")}</div>
                      {probe.failureDetail && (
                        <div className="mt-0.5 break-all opacity-90">{probe.failureDetail}</div>
                      )}
                      <button
                        onClick={() => {
                          finish();
                          onOpenLogs();
                        }}
                        className="mt-1 underline underline-offset-2 hover:opacity-80"
                      >
                        {t("onboarding.s4OpenLogs")}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>

          {/* 底部操作条 */}
          <div className="flex items-center gap-2 border-t border-border px-5 py-3">
            {step > 1 && (
              <Button size="sm" variant="outline" onClick={() => setStep((s) => s - 1)}>
                {t("onboarding.back")}
              </Button>
            )}
            <div className="flex-1" />
            {step === 2 && (
              <Button size="sm" disabled={!hasKey} onClick={() => setStep(3)}>
                {t("onboarding.next")} <ArrowRight size={14} />
              </Button>
            )}
            {step === 3 && (
              <Button size="sm" disabled={starting} onClick={() => void doStart()}>
                {starting ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />}
                {t("onboarding.s3Action")}
              </Button>
            )}
            {step === 4 && (
              <Button size="sm" onClick={finish}>
                <Check size={14} /> {t("onboarding.finish")}
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* 复用既有组件，不重写。KeyEditor 保存成功后直接推进到第③步：
          用户刚填完表单，不该再要求他点一次「下一步」。 */}
      {editorOpen && (
        <KeyEditor
          initial={null}
          onClose={() => setEditorOpen(false)}
          onSaved={() => setStep(3)}
        />
      )}
      {importOpen && (
        <CcSwitchImportDialog
          onClose={() => setImportOpen(false)}
          onImported={() => setImportOpen(false)}
        />
      )}
    </>,
    document.body,
  );
}
