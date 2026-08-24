import { describe, it, expect } from "vitest";
import { mkdtempSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// @ts-expect-error —— 纯 JS 门脚本，无类型声明；本文件不参与 tsc（tsconfig.include 只有 src）
import { scanFiles, scanRepo, looksLikeRealSecret } from "../scripts/lib/secret-scan.mjs";

/**
 * `scripts/lib/secret-scan.mjs` 的故障注入测试。
 *
 * ## 为什么这个门需要一份自己的测试
 *
 * 它防的那次事故（2026-08-14 更新签名私钥 + 口令进公开仓库）之所以没被既有的
 * `audit:release` 抓到，**不是因为没有门，而是因为门判错了维度**：它按文件名判，
 * 而密钥贴在 `.md` 正文里、还包了一层 base64。
 *
 * 一个判错维度的门和没有门是一样的，而且更糟 —— 它每次都绿，让人以为查过了。
 * 所以这里对每条判据都给一个**必须被抓到**的正例，以及一个**必须不被抓到**的负例。
 *
 * ## 夹具都在临时目录里现造，不入库
 *
 * 刻意不把假密钥放进 `tests/fixtures/`：那些文件会被 git 跟踪，于是真的那道门
 * 会对它们报警，接着就得加白名单 —— 而白名单正是下一次泄露的藏身处。
 * 现造现删，扫描器不需要任何自我豁免。
 *
 * 同理，本文件里的签名串一律**拼接**而成，不出现完整字面量。
 */

const UC = "untrusted comment: ";
const ENC_KEY_COMMENT = UC + "rsign encrypted " + "secret key";
const PLAIN_KEY_COMMENT = UC + "rsign " + "secret key";
const PEM_HEAD = "-----BEGIN " + "PRIVATE KEY-----";

/** 一份形态逼真的 rsign 密钥文件（内容是假的，只要形态对）。 */
const fakeKeyFile = `${ENC_KEY_COMMENT}\nRWRTY0Iy${"A".repeat(340)}\n`;

function withFixtures(files: Record<string, string>) {
  const dir = mkdtempSync(join(tmpdir(), "secret-scan-"));
  for (const [rel, content] of Object.entries(files)) {
    const abs = join(dir, rel);
    mkdirSync(join(abs, ".."), { recursive: true });
    writeFileSync(abs, content, "utf8");
  }
  try {
    return { dir, ...scanFiles(Object.keys(files), { cwd: dir }) };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const kinds = (findings: any[], kind: string) => findings.filter((f) => f.kind === kind);

describe("密钥材料扫描：必须抓到的形态", () => {
  it("base64 包装的私钥 —— 这次事故的真实形态，纯 grep 抓不到", () => {
    // `TAURI_SIGNING_PRIVATE_KEY` 就是「整份密钥文件再 base64 一层」，
    // 于是那句 comment 在文件里根本不以明文出现。判据必须先解码。
    const wrapped = Buffer.from(fakeKeyFile, "utf8").toString("base64");
    expect(wrapped).not.toContain("rsign"); // 前提：明文特征确实消失了
    const { findings } = withFixtures({ "GITHUB_SECRETS_SETUP.md": `**Value**:\n\n\`\`\`\n${wrapped}\n\`\`\`\n` });
    expect(kinds(findings, "key").map((f) => f.what).join()).toMatch(/base64 包装/);
  });

  it("原样贴进来的私钥文件（明文 comment）", () => {
    const { findings } = withFixtures({ "notes/key.txt": fakeKeyFile });
    expect(kinds(findings, "key").length).toBeGreaterThan(0);
  });

  it("明文形态（未加口令）的私钥同样要抓", () => {
    const { findings } = withFixtures({ "a.md": `${PLAIN_KEY_COMMENT}\nRWRTY0Iy${"B".repeat(300)}\n` });
    expect(kinds(findings, "key").length).toBeGreaterThan(0);
  });

  it("PEM 私钥", () => {
    const { findings } = withFixtures({ "id.txt": `${PEM_HEAD}\nMIIEvQ...\n-----END PRIVATE KEY-----\n` });
    expect(kinds(findings, "key").length).toBeGreaterThan(0);
  });

  it("只贴密钥主体那一行（没有 comment 行）也要抓", () => {
    const { findings } = withFixtures({ "b.md": `密钥：RWRTY0Iy${"C".repeat(300)}\n` });
    expect(kinds(findings, "key").map((f) => f.what).join()).toMatch(/主体/);
  });

  it("markdown 里「标签 → 空行 → **Value**: → 围栏 → 值」的口令（隔了 4 行）", () => {
    // 这是本次泄露的**逐字形态**。只看同一行的判据会漏掉它。
    const md = [
      "### 2. TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
      "",
      "**Name**: `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`",
      "",
      "**Value**: ",
      "```",
      "hunter2.SoSecret",
      "```",
    ].join("\n");
    const { findings } = withFixtures({ "SETUP.md": md });
    const p = kinds(findings, "passphrase");
    expect(p.length).toBe(1);
    expect(p[0].value).toBe("hunter2.SoSecret");
  });

  it("同行形态 GPG_PASSPHRASE=…", () => {
    const { findings } = withFixtures({ "Makefile": "GPG_PASSPHRASE=correct-horse-battery\n" });
    expect(kinds(findings, "passphrase").length).toBe(1);
  });
});

describe("假警防线：必须不报的形态", () => {
  it("脚本里以短字面量『提到』私钥前缀 —— audit-release-bundle.mjs 就是这样", () => {
    // 第一版若不要求 ≥200 字符的长串，本仓自己的两个门脚本会被判成泄露源。
    const src = `const needles = [["minisign 私钥内容", "RWRTY0Iy"]];\n`;
    const { findings } = withFixtures({ "scripts/x.mjs": src });
    expect(findings).toHaveLength(0);
  });

  it("CI 里用变量引用口令（release.yml 的真实形态）", () => {
    const yml = [
      "        env:",
      "          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}",
      "          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}",
      "        with:",
      "          projectPath: .",
      "          includeUpdaterJson: true",
    ].join("\n");
    const { findings } = withFixtures({ ".github/workflows/release.yml": yml });
    expect(findings).toHaveLength(0);
  });

  it("占位符值不报", () => {
    const md = "**Name**: `GPG_PASSPHRASE`\n\n**Value**:\n```\n<你的口令>\n```\n";
    const { findings } = withFixtures({ "doc.md": md });
    expect(findings).toHaveLength(0);
  });

  it("标签后面跟的是中文说明，不是值", () => {
    const md = [
      "设 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 这个环境变量。",
      "",
      "口令请存进密码管理器，不要写进仓库。",
    ].join("\n");
    const { findings } = withFixtures({ "docs/18.md": md });
    expect(findings).toHaveLength(0);
  });

  it("解出来是垃圾的长 base64 不报（sourcemap / 字体等）", () => {
    const junk = Buffer.from("x".repeat(400), "utf8").toString("base64");
    const { findings } = withFixtures({ "site-bundle.js": `const m="${junk}";\n` });
    expect(findings).toHaveLength(0);
  });

  it("looksLikeRealSecret 对标签名本身返回 null", () => {
    expect(looksLikeRealSecret("`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`")).toBeNull();
    expect(looksLikeRealSecret("${{ secrets.FOO }}")).toBeNull();
    expect(looksLikeRealSecret("mhm292117.")).toBe("mhm292117.");
  });
});

describe("门本身不能空转", () => {
  it("在真仓库上必须真的扫到文件、且真的走过 base64 解码那条路", () => {
    // 一个「解析到 0 个目标」的门永远绿、永远什么都没测 —— 本仓的
    // check-forbidden.mjs 第一版就栽在这（查 invoke() 命中 0 处）。
    const { stats } = scanRepo();
    expect(stats.scanned).toBeGreaterThan(50);
    expect(stats.b64Runs).toBeGreaterThan(0);
  });
});
