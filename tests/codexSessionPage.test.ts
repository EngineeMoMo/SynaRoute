// 会话页的两条跨语言判据。**编译器管不到这两条缝，而分叉的表现都是静默的。**
//
// 本仓已在同一类接线盲区上栽过十几次（`route_meta` / `lan_guard` 的 peer /
// `log_rotate` 的写线程 / `key_flags` 的 checkbox …）：Rust 侧的行为用例直调函数、
// 照样全绿，而前端那一行改坏了就是缺陷本体。

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { sessionsEn, sessionsZh } from "@/lib/i18n.sessions";

const page = readFileSync("src/pages/CodexSessionsPage.tsx", "utf8");
const rustOps = readFileSync("src-tauri/src/tools/codex_session_ops.rs", "utf8");

/** 剥掉 `//` 行注释：判据说「代码里必须/不许出现某形态」，那就只能看代码。 */
const code = (src: string) =>
  src
    .split("\n")
    .filter((l) => !l.trim().startsWith("//") && !l.trim().startsWith("*"))
    .join("\n");

describe("会话页与后端的两处契约", () => {
  /**
   * 🔴 「指向别处」这个数字只能有**一个**来源。
   *
   * 后端 `view::enrich` 逐行判定并累加 `stats.mismatched`，前端必须直接用它。前端自己再
   * `rows.filter(...)` 算一遍的话，两处口径迟早漂移，而漂移的表现是「表头说 3 条指向别处、
   * 表里只标红 1 行」—— 用户不知道该信哪个，而两个数字都看起来像是真的。
   */
  it("统计数字取自后端，前端不许自己再算一遍", () => {
    const src = code(page);
    expect(src, "必须直接用后端算好的那个数字").toContain("data?.stats.mismatched");
    expect(src, "「模型已失效」的条数同理").toContain("data?.stats.modelGone");
    // 反向：不许在页面里按 provider 重新过滤出「不一致」的行。
    expect(
      /rows\s*\.filter\([^)]*provider\s*!==/.test(src),
      "前端不许按 provider 自己再过滤一遍 —— 那就是第二个事实来源",
    ).toBe(false);
  });

  /**
   * 🔴 「立刻同步」**必须先弹确认**，不许直接落盘。
   *
   * 它改的是用户的对话文件，而同步目标现在是个可以任选的下拉（能指向任何 provider id）——
   * 误操作的代价比只有「同步到 synaroute」那一版高得多。本仓刚在大脑聚合那轮定下同一条纪律：
   * 让用户确认的必须是他看到的那份（那里为此把落盘拆成了 Phase2a 预览 + Phase2b 写入）。
   *
   * 判据钉「按钮点的是 setConfirmingSync，而 api.syncCodexSessions 只在确认那一步被调」——
   * 少了确认框的表现不是报错，而是**点一下就改了几百个文件**。
   */
  it("立刻同步必须经确认框，按钮本身不许直接调后端", () => {
    const src = code(page);
    expect(src, "同步按钮应当只是打开确认框").toContain("onClick={() => setConfirmingSync(true)}");
    // `doSync` 里那一处是唯一的调用点，且它由确认按钮触发。
    expect(src.split("api.syncCodexSessions(").length - 1, "同步只能有一个调用点").toBe(1);
    expect(src, "确认按钮才是真正执行的那个").toContain("onClick={() => void doSync()}");
    for (const key of ["sessions.syncConfirmTitle", "sessions.syncConfirmOk"]) {
      expect(src, `确认框缺 ${key}`).toContain(key);
    }
  });

  /**
   * 🔴 确认框里承诺的备份目录，必须**就是后端真的写进去的那个**。
   *
   * 删除是本应用唯一会主动删用户对话记录的地方，而这句文案是用户判断「删错了还能不能救」
   * 的唯一依据。后端把目录名改了而文案没跟上，用户会去一个不存在的目录里找他的对话 ——
   * 同本仓「摘掉一个信息源时必须把所有指向它的指路文案一起改」那条（401 响应体指向日志页，
   * 而令牌已经只剩指纹）。
   */
  it("删除确认文案里的备份目录与 Rust 侧一致", () => {
    const m = /const BACKUP_KIND: &str = "([^"]+)"/.exec(rustOps);
    expect(m, "没从 codex_session_ops.rs 解析到 BACKUP_KIND —— 判据空转了，先修解析").not.toBe(
      null,
    );
    const kind = m![1];
    for (const [lang, dict] of [
      ["zh", sessionsZh],
      ["en", sessionsEn],
    ] as const) {
      expect(dict["sessions.confirmBody"], `${lang} 的删除确认文案没提备份目录 ${kind}`).toContain(
        kind,
      );
    }
  });

  /**
   * 五个新命令都要在前端真的被调到。
   *
   * 策略门 `invoke-command-must-exist` 查的是「前端调的名字在 Rust 有定义」（正向），
   * Rust 侧那条 `the_sync_commands_must_be_registered_in_the_handler_list` 查的是注册。
   * 两者都不管**页面有没有真的接上按钮** —— 而「后端做好了、界面上点不到」这个形态本仓
   * 也发生过（`headers_json` 是纯后端死字段，文档却写着「Key 编辑器里那个输入框」）。
   */
  it("同步与索引清理的入口真的接在页面上", () => {
    const src = code(page);
    for (const fn of [
      "listCodexProviderTargets",
      "syncCodexSessions",
      "setCodexSessionAutoSync",
      "auditCodexSessionIndex",
      "pruneCodexSessionIndex",
    ]) {
      expect(src, `${fn} 没有任何调用点 —— 后端做好了但界面上点不到`).toContain(`api.${fn}(`);
    }
  });
});
