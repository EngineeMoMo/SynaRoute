import { useEffect, useState } from "react";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { Combobox } from "@/components/ui/Combobox";
import { AlertTriangle, ArrowRight, Brain, Gauge, Plus, Sparkles, Trash2, Zap } from "lucide-react";
import type {
  CategoryType,
  DesktopModelNameIssue,
  DesktopModelNameReport,
  ModelInfo,
  ModelMapping,
} from "@/types";

/**
 * 模型映射区：档位快捷映射（四档）+ 精确映射（真实名 → 对外名 · 显示名）。
 *
 * 从 `KeyEditor.tsx` 抽出来（那边冻结在棘轮上、余量为 0）。
 *
 * # 三格各是什么（这一区最容易被误解，故写在最前）
 *
 * | 格 | 语义 | 谁在看 |
 * |---|---|---|
 * | 真实模型 | 实际发给上游的名字 | 上游 |
 * | 对外名 | 客户端能接受、并且会**发回来**给我们的 id | 客户端的合规判据、路由日志 |
 * | 显示名 | 客户端菜单上的文字 | 用户 |
 *
 * 用户只需要动第一格：对外名自动生成，显示名留空即等于真实模型名 ——
 * 那正是「菜单里要看到真的模型名字」这个诉求的零配置解。
 *
 * # 🔴 为什么样式常量在这里又写了一份
 *
 * 不从 `KeyEditor` 导入 `inputCls`：`KeyEditor` 要 import 本组件，反向再导入就是**模块循环**，
 * 而 React 组件在循环里可能在求值时还是 undefined（偶发、难查）。本仓抽 `ToggleRow` 时
 * 真踩过这一条。既有的 `CustomHeadersField` 同样是自己写样式。
 */
const inputCls =
  "h-9 w-full rounded-control border border-border bg-surface px-3 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring";

/** 四档的当前值。字段名与 `ProviderKey` 的 `tier*` 一一对应。 */
export interface TierValues {
  haiku: string;
  sonnet: string;
  opus: string;
  fable: string;
}

interface Props {
  /** 编辑中这条 Key 的分类。决定档位区是否显示、以及对外名怎么生成。 */
  category: CategoryType;
  /** 可选的真实模型（下拉候选）。 */
  models: ModelInfo[];
  mappings: ModelMapping[];
  setMappings: React.Dispatch<React.SetStateAction<ModelMapping[]>>;
  tiers: TierValues;
  onTierChange: (which: keyof TierValues, value: string) => void;
  /**
   * 对外名体检。由 `KeyEditor` 提供 —— 它才持有完整草稿，而体检的输入源必须是
   * `serviceable_models()`（与保存拦截同一个集合），两边各拼一份会出现
   * 「界面说没问题、保存却被拒」的自相矛盾。
   *
   * 传入要体检的那份 `mappings`：常规体检传当前值，求「对外名建议」时传
   * 一份把 `expectedName` 置成 `realName` 的探测副本。
   */
  probe: (mappings: ModelMapping[]) => Promise<DesktopModelNameReport>;
}

/**
 * 异步建议回来了：把那一行的对外名换成合规建议。
 *
 * 🔴 **只在它仍是我们当初乐观填进去的那个值时才换**。用户在那 250ms 里完全可能已经
 * 手改过对外名（点完模型顺手就去改是很自然的操作），无条件覆盖就是「刚打的字被吃掉」——
 * 而且他不会知道是谁改的。同理用 `rowId` 定位而不是索引：这一步落地时他可能已经删过别的行。
 *
 * 抽成纯函数是为了能被测到 —— 本仓没有 jsdom，留在 `.then()` 里的逻辑一律零覆盖。
 */
export function adoptSuggestion(
  rows: ModelMapping[],
  rowId: string,
  optimistic: string,
  suggestion: string,
): ModelMapping[] {
  return rows.map((m) =>
    m.id === rowId && m.expectedName === optimistic ? { ...m, expectedName: suggestion } : m,
  );
}

/** 桌面端分类才有对外名合规约束（后端 `CategoryMeta::strict_model_id`）。 */
const isStrict = (c: CategoryType) => c === "claude-desktop";

/**
 * 选/改了某一行的「真实模型」之后，那一行该变成什么样。
 *
 * 抽成**纯函数**是为了能被测到：本仓没有 jsdom/testing-library，组件内部的箭头函数
 * 一律零覆盖，而「自动生成对外名」正是本轮用户能直接感知的那一半。
 *
 * - `row`：更新后的映射行。对外名为空时**乐观填上真实名**；已有值则一个字节都不动 ——
 *   🔴 「自动生成 + 可改」的语义就是不能反过来吃掉用户的修改。
 * - `needsSuggestion`：还要不要再异步问后端要一个**合规**对外名。只有桌面端分类需要
 *   （CLI/Codex 没有合规约束，`to_gateway_model_id` 会自动包一层前缀，
 *   而对外名 == 真实名对用户是最直观的形态）。
 */
export function applyRealNameChange(
  row: ModelMapping,
  realName: string,
  strict: boolean,
): { row: ModelMapping; needsSuggestion: boolean } {
  const trimmed = realName.trim();
  if (row.expectedName.trim() || !trimmed) {
    return { row: { ...row, realName }, needsSuggestion: false };
  }
  return { row: { ...row, realName, expectedName: trimmed }, needsSuggestion: strict };
}

/**
 * 一键修法：给 `models` 里的**每一个**模型都建一条映射。
 *
 * **必须是每一个，不能只给不合规的建** —— `serviceable_models()` 的语义是「只要存在任意一条
 * 完整映射，models 列表就被整份忽略」。若只给 glm-4.6 建映射，同列表里本来合规的
 * claude-opus-4-8 会直接从桌面端选择器里**消失**，用户「修好一个问题、丢了一个模型」，
 * 而且没有任何提示。合规的那些建 realName → realName 的恒等映射即可。
 * （model.rs 的 applying_report_suggestions_makes_key_saveable 用故障注入钉住了这条。）
 *
 * 🔴 **必须保留用户已填的显示名**：这个按钮是整份重建 mappings，而重建时丢掉
 * `displayName` 的表现是「点一下一键修法，之前写好的菜单显示名全没了」—— 静默且不可撤销。
 * 按 realName 认领旧行：那是这两份数据唯一稳定的对应关系（id 会重新生成、对外名会被改写）。
 *
 * id 带 `_${i}`：批量生成会落在同一毫秒，只用 `now` 会撞号 —— React key 重复，
 * 且按 id 删除时会一次删掉多条。
 */
export function rebuildMappingsForAllModels(
  models: ModelInfo[],
  existing: ModelMapping[],
  issues: DesktopModelNameIssue[],
  now: number,
): ModelMapping[] {
  const keptLabel = new Map(
    existing.filter((m) => m.displayName?.trim()).map((m) => [m.realName.trim(), m.displayName]),
  );
  return models.map((m, i) => ({
    id: `m_${now}_${i}`,
    expectedName: issues.find((x) => x.name === m.realName)?.suggestion ?? m.realName,
    realName: m.realName,
    displayName: keptLabel.get(m.realName.trim()),
  }));
}

export function ModelMappingSection({
  category,
  models,
  mappings,
  setMappings,
  tiers,
  onTierChange,
  probe,
}: Props) {
  const t = useT();
  const [desktopIssues, setDesktopIssues] = useState<DesktopModelNameIssue[]>([]);
  const modelNames = models.map((m) => m.realName);

  // 对外名体检：只有桌面端分类适用，其余分类后端返回 applicable=false（前端一条都不显示）。
  // 250ms 防抖 —— 用户在输入框里连打时不该每个字符发一次 IPC。
  useEffect(() => {
    if (!isStrict(category)) {
      setDesktopIssues([]);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      void probe(mappings)
        .then((r) => {
          if (!cancelled) setDesktopIssues(r.applicable ? r.issues : []);
        })
        .catch(() => {
          if (!cancelled) setDesktopIssues([]);
        });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [category, mappings, models, tiers]);

  /** 是否已有「填完整」的映射行——决定 serviceable_models 走映射还是走 models 列表。 */
  const hasEffectiveMapping = mappings.some((m) => m.expectedName.trim() && m.realName.trim());

  /** 把某一行映射的对外名改成建议值。 */
  const applySuggestion = (rowId: string, suggestion: string) =>
    setMappings((prev) => prev.map((m) => (m.id === rowId ? { ...m, expectedName: suggestion } : m)));

  /**
   * 一键修法：见 [`rebuildMappingsForAllModels`]（规则在那里，这里只接线）。
   */
  const fixAllByAddingMappings = () =>
    setMappings((prev) => rebuildMappingsForAllModels(models, prev, desktopIssues, Date.now()));

  /**
   * 加一行空映射。用户接下来选真实模型，对外名由 [`changeRealName`] 自动生成。
   *
   * `displayName: undefined` **显式写出来**（而不是省略，两者运行时等价）：
   * 有一条跨语言判据要求「整份构造映射行的地方字段必须齐全」，
   * 而齐全的意义在于 —— 日后 `ModelMapping` 加第五个字段时，这里会被判据逼着一起改。
   * 省略的写法在那一天是静默漏掉。
   */
  const addMapping = () =>
    setMappings((prev) => [
      ...prev,
      { id: `m_${Date.now()}`, expectedName: "", realName: "", displayName: undefined },
    ]);

  /**
   * 改某一行的「真实模型」。决策规则在纯函数 [`applyRealNameChange`] 里，这里只做
   * 「接线 + 那一次异步补全」。
   */
  const changeRealName = (rowId: string, val: string) => {
    const trimmed = val.trim();
    let needsSuggestion = false;
    setMappings((prev) =>
      prev.map((m) => {
        if (m.id !== rowId) return m;
        const r = applyRealNameChange(m, val, isStrict(category));
        needsSuggestion = r.needsSuggestion;
        return r.row;
      }),
    );
    if (!needsSuggestion) return;
    // 异步换成合规建议。用 rowId 定位而不是索引：这一步落地时用户可能已经删过别的行。
    void probe(
      mappings.map((m) =>
        m.id === rowId
          ? { ...m, realName: val, expectedName: trimmed }
          : { ...m, expectedName: m.realName },
      ),
    )
      .then((r) => {
        const sug = r.issues.find((x) => x.name === trimmed)?.suggestion;
        if (!sug) return;
        setMappings((prev) => adoptSuggestion(prev, rowId, trimmed, sug));
      })
      .catch(() => {
        /* 拿不到建议就把乐观值留着：保存时会被拦下并给出修法，不比现状差 */
      });
  };

  const TIERS: { key: keyof TierValues; icon: typeof Zap; label: string; ph: string }[] = [
    { key: "haiku", icon: Zap, label: t("editor.tierHaiku"), ph: "glm-4.5-air" },
    { key: "sonnet", icon: Gauge, label: t("editor.tierSonnet"), ph: "glm-4.6" },
    { key: "opus", icon: Brain, label: t("editor.tierOpus"), ph: "deepseek-reasoner" },
    { key: "fable", icon: Sparkles, label: t("editor.tierFable"), ph: "gpt-5.6-sol" },
  ];

  return (
    <>
      {/* 档位快捷映射（取自 cc-switch 的 haiku/sonnet/opus/fable 语义，落到运行时代理）。
          仅 Claude CLI / 桌面端有意义：Claude Code 按任务发带档位子串的模型名才触发改写。
          Codex 发 GPT 名匹配不到档位，且若 models 里有 claude-*opus* 之类名字反而会被误改写 → 故 Codex 隐藏。 */}
      {category !== "codex" && (
        <div>
          <div className="mb-1.5">
            <span className="text-xs font-medium text-text-secondary">{t("editor.tierTitle")}</span>
            <p className="mt-0.5 text-[11px] leading-relaxed text-text-muted">{t("editor.tierHint")}</p>
          </div>
          <div className="space-y-1.5">
            {TIERS.map((tier) => {
              const Icon = tier.icon;
              return (
                <div key={tier.key} className="flex items-center gap-1.5">
                  <span className="flex w-24 shrink-0 items-center gap-1 text-xs text-text-secondary">
                    <Icon size={13} className="text-text-muted" /> {tier.label}
                  </span>
                  <ArrowRight size={14} className="shrink-0 text-text-muted" />
                  <div className="flex-1">
                    <Combobox
                      className={`${inputCls} font-mono`}
                      value={tiers[tier.key]}
                      options={modelNames}
                      placeholder={tier.ph}
                      emptyHint={t("editor.comboNoModels")}
                      onChange={(v) => onTierChange(tier.key, v)}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* 模型映射 */}
      <div>
        <div className="mb-1.5 flex items-center justify-between">
          <span className="text-xs font-medium text-text-secondary">{t("editor.mappingTitle")}</span>
          <Button size="sm" variant="ghost" onClick={addMapping}>
            <Plus size={13} /> {t("editor.addMapping")}
          </Button>
        </div>
        <p className="mb-1.5 text-[11px] leading-relaxed text-text-muted">{t("editor.mappingHint")}</p>
        <div className="space-y-1.5">
          {/* 批量警告条（UX#4）：只在「还没配任何有效映射」时出——此时用户的对外名就是
              models 里的真实名，一个都不合规，逐行提示会刷屏，给一键修法才是正解。
              一旦有了映射，就转为逐行提示（下面那段），因为那时问题是具体某一行填错了。 */}
          {desktopIssues.length > 0 && !hasEffectiveMapping && (
            <div className="space-y-1.5 rounded-control border border-warning/40 bg-warning/10 px-2.5 py-2">
              <div className="flex items-start gap-2 text-[11px] leading-relaxed text-warning">
                <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                <span className="flex-1">{t("editor.desktopNameBadBanner", { n: desktopIssues.length })}</span>
              </div>
              <div className="text-[11px] leading-relaxed text-text-muted">
                {t("editor.desktopNameFixAllHint")}
                <br />
                {t("editor.desktopNamePrefixUseless")}
              </div>
              <button
                type="button"
                onClick={fixAllByAddingMappings}
                className="rounded border border-warning/40 px-2 py-0.5 text-[11px] font-medium text-warning hover:bg-warning/20"
              >
                {t("editor.desktopNameFixAll", { n: models.length })}
              </button>
            </div>
          )}
          {mappings.length === 0 && <span className="text-xs text-text-muted">{t("editor.noMapping")}</span>}
          {mappings.length > 0 && (
            <div className="flex items-center gap-1.5 text-[10px] text-text-muted">
              <span className="flex-[4]">{t("editor.mappingColReal")}</span>
              <span className="w-3.5 shrink-0" />
              <span className="flex-[4]">{t("editor.mappingColOutward")}</span>
              <span className="flex-[3]">{t("editor.mappingColDisplay")}</span>
              <span className="w-7 shrink-0" />
            </div>
          )}
          {mappings.map((m) => {
            // 逐行提示：只有「这一行的对外名」出现在体检结果里才提示。
            // realName 还空着的行不会进 serviceable_models，也就不会有 issue —— 刻意如此，
            // 否则用户填了一半就被警告、而保存其实会成功，属于假警报。
            const issue = desktopIssues.find((x) => x.name === m.expectedName.trim());
            return (
              <div key={m.id} className="space-y-1">
                <div className="flex items-center gap-1.5">
                  <div className="flex-[4]">
                    <Combobox
                      className={`${inputCls} font-mono`}
                      value={m.realName}
                      options={modelNames}
                      placeholder="glm-5.3"
                      emptyHint={t("editor.comboNoModels")}
                      onChange={(val) => changeRealName(m.id, val)}
                    />
                  </div>
                  <ArrowRight size={14} className="shrink-0 text-text-muted" />
                  <input
                    className={`${inputCls} flex-[4] font-mono ${issue ? "border-warning" : ""}`}
                    placeholder="claude-sonnet-5-3"
                    value={m.expectedName}
                    onChange={(e) =>
                      setMappings((prev) =>
                        prev.map((x) => (x.id === m.id ? { ...x, expectedName: e.target.value } : x)),
                      )
                    }
                  />
                  {/* 显示名：placeholder 动态显示真实模型名 —— 让「留空就是它」这件事不用读说明也能看懂。 */}
                  <input
                    className={`${inputCls} flex-[3] font-mono`}
                    placeholder={m.realName.trim() || t("editor.mappingColDisplay")}
                    value={m.displayName ?? ""}
                    onChange={(e) =>
                      setMappings((prev) =>
                        prev.map((x) =>
                          x.id === m.id ? { ...x, displayName: e.target.value || undefined } : x,
                        ),
                      )
                    }
                  />
                  <button
                    onClick={() => setMappings((prev) => prev.filter((x) => x.id !== m.id))}
                    className="shrink-0 rounded p-1.5 text-text-muted hover:bg-surface-hover hover:text-danger"
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
                {issue && (
                  <div className="flex items-start gap-2 rounded-control bg-warning/10 px-2 py-1 text-[11px] leading-relaxed text-warning">
                    <AlertTriangle size={11} className="mt-0.5 shrink-0" />
                    <span className="flex-1">{t("editor.desktopNameBadRow", { name: issue.name })}</span>
                    <button
                      type="button"
                      onClick={() => applySuggestion(m.id, issue.suggestion)}
                      className="shrink-0 whitespace-nowrap rounded border border-warning/40 px-1.5 py-0.5 font-medium hover:bg-warning/20"
                    >
                      {t("editor.desktopNameFixTo", { name: issue.suggestion })}
                    </button>
                  </div>
                )}
              </div>
            );
          })}
          {mappings.some((m) => m.displayName?.trim()) && (
            <p className="text-[11px] leading-relaxed text-text-muted">{t("editor.mappingDisplayNote")}</p>
          )}
        </div>
      </div>
    </>
  );
}
