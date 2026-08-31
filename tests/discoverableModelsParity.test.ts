import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { discoverableModels, keyExpectedSet } from "../src/lib/modelSets";
import type { ProviderKey } from "../src/types";

/**
 * 对外可选模型清单的**跨语言不变量**。
 *
 * 同一个口径有两份实现，编译器管不到它们之间的缝：
 * - Rust `model_pool::discoverable_models` —— `GET /v1/models`、写进三端客户端配置、
 *   Codex 模型目录、以及转发时「这个名字我们宣称过吗」那道判据
 * - TS `discoverableModels` —— 状态条「当前模型」下拉与分类页
 *
 * 分叉是**静默**的：界面列出一个后端没宣称过的名字，用户选了它，转发时
 * `reject_if_unserviceable` 把它当成「客户端自己编的」放过 → 静默降级到别的模型。
 * 用户选了 A 拿到 B 的回答，而日志里只是一行正常的「兜底改写」。
 * `modelSets.ts` 自己的注释一直写着这个风险，但直到 2026-08-31 才有机械判据。
 *
 * 判据取自 **Rust 源码里的实现形态本身**，而不是「我记得它是并集」—— 任何一侧改回
 * 交集、或改变顺序规则，这里必须变红。
 *
 * 放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码，理由同 mcpEndpointParity.test.ts。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const POOL_RS = readFileSync(join(ROOT, "src-tauri", "src", "model_pool.rs"), "utf8");

/**
 * 只看生产段、且剥掉 `//` 系注释。
 *
 * 本仓已 5 次栽在「注释里的字面量满足了断言」上（`data-dir-env-name-must-match` /
 * `userPrefsParity` / `only_v6_must_be_set_explicitly` / `check-tailwind-tokens` /
 * 一个一次性扫查脚本）。判据说「代码里别这么写」，就只能看代码。
 *
 * 🔴 **先归一行尾**：`.rs` 不在 `.gitattributes` 的管辖里，仓库存 LF 而
 * `core.autocrlf=true`（本机就是）会让**下一次 clone / checkout** 得到 CRLF。那时
 * `indexOf("#[cfg(test)]\nmod tests")` 静默匹配不到 → 切不掉测试段 → 下面那条「交集残留
 * 形态」判据会扫到测试段里 `!src.contains("backup_sets")` 的**字符串字面量**并报假警。
 * 同 CLAUDE.md 里 `codex_base_instructions.md` 的 20903 字节与 `Cargo.lock` 那两条坑。
 */
function prodOnly(src: string): string {
  const normalized = src.replace(/\r\n/g, "\n");
  const cut = normalized.indexOf("#[cfg(test)]\nmod tests");
  return (cut >= 0 ? normalized.slice(0, cut) : normalized)
    .split("\n")
    .map((line) => {
      const i = line.indexOf("//");
      return i >= 0 ? line.slice(0, i) : line;
    })
    .join("\n");
}

function key(
  id: string,
  priority: number,
  models: string[],
  mappings: [string, string][] = []
): ProviderKey {
  return {
    id,
    categoryId: "claude-cli",
    name: id,
    vendor: "custom",
    baseUrl: "https://api.example.com",
    protocol: "anthropic",
    hasSecret: true,
    enabled: true,
    priority,
    params: {},
    models: models.map((realName) => ({ realName, source: "manual" as const })),
    mappings: mappings.map(([expectedName, realName]) => ({
      id: `${expectedName}->${realName}`,
      expectedName,
      realName,
    })),
    health: { status: "unknown", failCount: 0 },
  };
}

describe("对外模型清单：前后端同口径", () => {
  it("Rust 侧仍是并集实现，且没有交集的残留形态", () => {
    const prod = prodOnly(POOL_RS);
    expect(
      prod.includes("pub(crate) fn discoverable_models"),
      "model_pool.rs 里找不到 discoverable_models —— 解析器坏了还是函数搬走了？"
    ).toBe(true);

    // 并集的形态：遍历**每一条** Key，逐个 push 去重。
    expect(prod).toMatch(/for key in candidates \{[\s\S]*?for name in key\.serviceable_models\(\)/);

    // 🔴 交集的三种残留形态。任何一种回来都意味着备用 Key 独有的模型又被藏了起来，
    // 而那正是用户报的「多 Key 时能选的模型太少」。
    for (const banned of ["backup_sets", "split_first", "intersection"]) {
      expect(prod.includes(banned), `交集实现的残留形态 \`${banned}\` 回来了`).toBe(false);
    }
  });

  it("前端拼出的清单与 Rust 的并集规则逐字一致", () => {
    // 与 Rust 侧 `the_union_keeps_the_primary_first_and_appends_the_rest` 同一组夹具。
    const a = key("a", 0, ["opus", "sonnet"]);
    const b = key("b", 1, ["sonnet", "glm"]);
    const c = key("c", 2, ["glm", "kimi"]);
    expect(discoverableModels([a, b, c])).toEqual(["opus", "sonnet", "glm", "kimi"]);

    // 顺序是契约：首个会被写进 env.ANTHROPIC_MODEL、Codex 目录挑默认模型、桌面端默认项。
    // 传入顺序打乱后结果必须不变（内部按 priority 排）。
    expect(discoverableModels([c, a, b])).toEqual(["opus", "sonnet", "glm", "kimi"]);
  });

  it("对外名完全不重合时两边的模型都在（交集口径下会只剩主 Key 的）", () => {
    const a = key("a", 0, ["claude-opus-4-7"]);
    const b = key("b", 1, ["glm-4.6"]);
    expect(discoverableModels([a, b])).toEqual(["claude-opus-4-7", "glm-4.6"]);
  });

  it("有映射时只暴露对外名，与 Rust serviceable_models 的第 1 条规则一致", () => {
    const k = key("a", 0, ["glm-4.6", "glm-4.5"], [["opus-4-8", "glm-4.6"]]);
    expect(discoverableModels([k])).toEqual(["opus-4-8"]);
    expect([...keyExpectedSet(k)]).toEqual(["opus-4-8"]);
  });

  it("已配三档追加 Claude 家族代表名，与 Rust 第 3 条规则一致", () => {
    const k = { ...key("a", 0, []), tierOpus: "deepseek-reasoner", tierHaiku: "glm-4.5-air" };
    // Rust 侧的追加顺序是 opus → sonnet → haiku（`serviceable_models` 里写死的）。
    expect(discoverableModels([k])).toEqual(["claude-opus-4-5", "claude-haiku-4-5"]);
  });

  it("没有 Key 就没有模型", () => {
    expect(discoverableModels([])).toEqual([]);
  });

  it("前端实现里不许留交集形态", () => {
    const ts = readFileSync(join(ROOT, "src", "lib", "modelSets.ts"), "utf8");
    const fn = ts.slice(ts.indexOf("export function discoverableModels"));
    const body = fn.slice(0, fn.indexOf("\n}"));
    expect(
      body.includes("every("),
      "`backups.every((s) => s.has(m))` 是交集实现的形态 —— 它回来就等于清单又被收窄了"
    ).toBe(false);
  });
});
