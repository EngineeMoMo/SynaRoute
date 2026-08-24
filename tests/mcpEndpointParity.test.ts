import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { mcpCategoryUrl } from "../src/components/McpAddressList";

/**
 * MCP 接入地址的**跨语言不变量**。
 *
 * 分类身份靠 url 路径段携带（`/mcp/<分类>`），写侧有两份实现：
 * - Rust `mcp::client_url` —— 写进客户端配置的那份（注册 / 端口漂移重写）
 * - TS `mcpCategoryUrl` —— 设置页复制框与接入向导给用户手工配置用的那份
 *
 * 两份分叉的表现是**静默**的：用户照界面上的地址手工配好，客户端连得上、工具能调，
 * 只是服务端认不出调用方，一律退回兜底分类（用错 Key 池、日志落错页），毫无提示。
 * 编译器管不到跨语言的这条缝，故用一条机械判据钉住。
 *
 * 判据取自 **Rust 源码里的格式串本身**，而不是「我记得它长这样」——
 * 把方案改成 `?category=` 之类的另一种携带方式时，这条必须变红。
 *
 * 放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码，理由同 silentClassNoop.test.ts。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const MCP_RS = readFileSync(join(ROOT, "src-tauri", "src", "mcp.rs"), "utf8");
const MODEL_RS = readFileSync(join(ROOT, "src-tauri", "src", "model.rs"), "utf8");

/** 前端展示的三个分类（与 McpAddressList 的 CATEGORIES 同源）。 */
const CATEGORIES = ["claude-cli", "codex", "claude-desktop"] as const;

describe("MCP 接入地址：前后端同形", () => {
  it("Rust 侧的路径方案仍是「基址 + / + 分类」", () => {
    const mcpPath = MCP_RS.match(/const MCP_PATH: &str = "([^"]+)";/);
    expect(mcpPath, "mcp.rs 里找不到 MCP_PATH —— 解析器坏了还是常量改名了？").toBeTruthy();
    expect(mcpPath![1]).toBe("/mcp");

    const base = MCP_RS.match(/fn base_url\(port: u16\) -> String \{\s*format!\("([^"]+)"\)/);
    expect(base, "mcp.rs 里找不到 base_url 的格式串").toBeTruthy();
    expect(base![1]).toBe("http://127.0.0.1:{port}{MCP_PATH}");

    // 关键判据：分类必须作为**路径段**追加。改成 `{}?category={}` 这类携带方式时这里变红，
    // 迫使改的人同时更新前端那份（否则界面给出的地址会静默失效）。
    const client = MCP_RS.match(
      /fn client_url\(port: u16, category: CategoryType\) -> String \{\s*format!\("([^"]+)", base_url\(port\), category\.as_str\(\)\)/
    );
    expect(client, "mcp.rs 里找不到 client_url 的格式串").toBeTruthy();
    expect(client![1]).toBe("{}/{}");
  });

  it("前端拼出的地址与 Rust 侧逐字一致", () => {
    for (const port of [9527, 47100, 65535]) {
      for (const c of CATEGORIES) {
        expect(mcpCategoryUrl(port, c)).toBe(`http://127.0.0.1:${port}/mcp/${c}`);
      }
    }
  });

  it("三条地址互不相同、且都不等于裸基址", () => {
    const base = "http://127.0.0.1:9527/mcp";
    const urls = CATEGORIES.map((c) => mcpCategoryUrl(9527, c));
    expect(new Set(urls).size, "三个分类的地址必须互不相同").toBe(CATEGORIES.length);
    for (const u of urls) {
      expect(u, "带分类段的地址不得等于裸基址（那就丢了身份）").not.toBe(base);
    }
  });

  it("前端用的分类字符串都是 Rust 侧真实的 wire_id", () => {
    // 打错一个字母（claude-desktp）不会有编译错误，只会让服务端把它判成「认不出的调用方」。
    const wireIds = new Set(
      [...MODEL_RS.matchAll(/wire_id: "([^"]+)"/g)].map((m) => m[1])
    );
    expect(wireIds.size, "model.rs 里解析到 0 个 wire_id —— 解析器坏了").toBeGreaterThan(0);
    for (const c of CATEGORIES) {
      expect(wireIds.has(c), `${c} 不是 Rust 侧的 wire_id（拼错了？）`).toBe(true);
    }
    // 反向：Rust 有几个分类，前端就该列几条，别漏掉新增的分类。
    expect(CATEGORIES.length, "分类数量与 Rust 侧不一致（新增分类忘了加进接入地址列表？）").toBe(
      wireIds.size
    );
  });
});
