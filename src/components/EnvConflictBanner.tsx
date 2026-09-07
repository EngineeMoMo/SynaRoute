// 「客户端环境变量顶掉了我们写的配置」这条常驻告警。
//
// 为什么是独立组件而不是塞进 CategoryPage：那个文件已经有三条常驻告警 + 两套轮询，
// 而这一条自带取数、自带「移除」这个**会改用户系统设置**的动作（要确认框 + 结果回显）。
//
// 🔴 **只 banner `conflict`，不 banner `notice`**：`OPENAI_*` 落在 notice 上，它对我们的
// Codex 路径通常无害（取证见 Rust 侧模块头）。把它挂到每个装过 OpenAI SDK 的用户脸上，
// 就是本仓最忌的那种假警 ——「指错方向的提示比没有提示更糟」。notice 只进诊断报告。

import { useEffect, useRef, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { CategoryType, EnvFinding } from "@/types";

/** 这条发现与用户正在看的分类相关吗。 */
function relevant(f: EnvFinding, category: CategoryType): boolean {
  if (f.severity !== "conflict") return false;
  // 🔴 **桌面端刻意不报 `ANTHROPIC_*`。** 它压根不读那些环境变量 —— 接入桌面端走的是
  // `deploymentMode=3p` + `configLibrary/{ID}.json`（`tools.rs` 模块头有取证：早期往
  // `claude_desktop_config.json` 写 `baseUrl` 从未生效，因为桌面端「无 baseUrl 概念」）。
  // 在桌面端页面上报它，等于叫用户去删一个与他的故障毫无关系的系统设置，而真正的成因
  // （部署模式没切、配置档没写进去）被这条告警盖住 ——「指错方向的提示比没有提示更糟」。
  if (f.name.startsWith("ANTHROPIC_")) return category === "claude-cli";
  if (f.name.startsWith("CODEX_")) return category === "codex";
  return false;
}

export function EnvConflictBanner({ category }: { category: CategoryType }) {
  const t = useT();
  const [found, setFound] = useState<EnvFinding[]>([]);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  /**
   * 当前分类的 ref。
   *
   * 🔴 **`doRemove` 里不能拿 `category` 当「现在是哪个分类」** —— 那是**闭包捕获的值**，
   * `await` 之后它仍是这一趟开始时的那个，于是 `started !== category` 恒为 false，
   * 那道守卫等于没写（我第一版就是这么写的）。ref 的 `.current` 才读得到最新值。
   */
  const currentRef = useRef(category);
  currentRef.current = category;

  // 一次性取数，不轮询：环境变量是低频事件，而这一趟要读注册表。
  // 切分类时重取（用户可能刚在另一页看到过），依赖数组里因此有 category。
  useEffect(() => {
    let alive = true;
    // 🔴 **切分类必须先清掉上一个分类的结果与提示。**
    //
    // 本组件不随分类重新挂载（只是换了个 prop），而 `note` 与 `found` 都是长驻 state。
    // 不清的表现有两种，都是「界面撒谎」：
    // ① `note` 参与渲染判据（`found.length === 0 && !note` 才返回 null）→ 在 claude-cli
    //    上点过「移除」之后切到 codex，那条横幅**照旧显示**，写着 claude-cli 那次的结果，
    //    而 codex 这边压根没有任何问题；
    // ② 新一趟取数返回前，旧分类的 `found` 还挂在屏幕上（同本仓「切分类时在途 IPC 把上一个
    //    分类的 Key 写进新分类页」那条 P0 的形状，只是这里危害是误导而不是数据错乱）。
    setNote("");
    setFound([]);
    setConfirming(false);
    setBusy(false);
    void api
      .detectEnvConflicts()
      .then((all) => {
        if (alive) setFound(all.filter((f) => relevant(f, category)));
      })
      .catch(() => {
        // 读不出来（非 Windows、注册表被策略挡住）不影响主界面
      });
    return () => {
      alive = false;
    };
  }, [category]);

  if (found.length === 0 && !note) return null;

  // 🔴 只有 `removable` 的才配得上一个「移除」按钮。目录变量（CODEX_HOME 等）报得出来、
  // 但**不该删** —— 那要对齐两侧。这一位来自后端，与它移除时用的白名单是同一个事实来源；
  // 各判一遍的表现是「按钮能点、点了返回『没有可移除的项』」。
  const removable = found.filter((f) => f.removable);

  const doRemove = async () => {
    setBusy(true);
    setConfirming(false);
    // 这一趟开始时的分类。移除 + 重新探测要走两次 IPC（其中一次还读注册表），期间用户完全
    // 可能切走 —— 那时下面的 `setFound`/`setNote` 会把**这个**分类的结果写进另一个分类的页面。
    const started = category;
    try {
      const r = await api.removeEnvConflicts(removable.map((f) => f.name));
      if (started !== currentRef.current) return;
      setNote(
        [
          r.removed.length > 0 ? t("env.removed", { n: r.removed.length }) : "",
          r.failed.join("；"),
          r.note,
          r.backupPath ? t("env.backupAt", { path: r.backupPath }) : "",
        ]
          .filter(Boolean)
          .join(" "),
      );
      const fresh = await api.detectEnvConflicts();
      if (started !== currentRef.current) return;
      setFound(fresh.filter((f) => relevant(f, started)));
    } catch (e) {
      if (started === currentRef.current) setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-6 mb-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-xs text-warning">
      {found.length > 0 && (
        <div className="flex items-start gap-2">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <div className="flex-1 leading-relaxed">
            <p className="font-medium">{t("env.title", { n: found.length })}</p>
            <ul className="mt-1 space-y-1">
              {found.map((f) => (
                <li key={`${f.source}:${f.name}`}>
                  <span className="font-mono">{f.name}</span>
                  {f.value ? `=${f.value}` : t("env.valueHidden")}
                  <span className="opacity-70">
                    {" "}
                    ({f.source === "user" ? t("env.srcUser") : t("env.srcProcess")})
                  </span>
                  <br />
                  {f.note}
                </li>
              ))}
            </ul>
            {/* 报了但不给删的那些（目录类）必须解释清楚 —— 否则用户会找一个不存在的按钮 */}
            {removable.length < found.length && (
              <p className="mt-1 opacity-80">{t("env.keepHint")}</p>
            )}
            {!confirming && removable.length > 0 && (
              <button
                type="button"
                disabled={busy}
                onClick={() => setConfirming(true)}
                className="mt-2 rounded-md border border-warning/50 px-2 py-1 disabled:opacity-50"
              >
                {t("env.remove", { n: removable.length })}
              </button>
            )}
            {/* 改的是用户的**系统环境**，不是我们自己的配置 —— 必须确认，且说清备份与生效条件 */}
            {confirming && (
              <div className="mt-2">
                <p>{t("env.confirmBody")}</p>
                <div className="mt-1.5 flex gap-2">
                  <button
                    type="button"
                    onClick={() => void doRemove()}
                    className="rounded-md bg-warning/12 border border-warning px-2 py-1 font-medium"
                  >
                    {t("env.confirmOk")}
                  </button>
                  <button
                    type="button"
                    onClick={() => setConfirming(false)}
                    className="rounded-md border border-warning/50 px-2 py-1"
                  >
                    {t("sessions.cancel")}
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>
      )}
      {note && <p className="mt-1 leading-relaxed opacity-90">{note}</p>}
    </div>
  );
}
