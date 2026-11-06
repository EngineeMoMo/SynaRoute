import { describe, expect, it } from "vitest";
import { readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { comparisonRef } from "../scripts/lib/ratchet-base.mjs";

const sha = "a".repeat(40);
const git = () => "v0.1.77\n";

describe("CI ratchet baseline", () => {
  it("uses the event's base rather than the checkout itself", () => {
    expect(comparisonRef({}, {}, git)).toBe("HEAD");
    expect(comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "push" }, { before: sha }, git)).toBe(sha);
    expect(comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "pull_request" }, { pull_request: { base: { sha } } }, git)).toBe(sha);
    expect(comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "push", GITHUB_REF: "refs/tags/v0.1.78" }, { before: "0".repeat(40) }, (args: string[]) => {
      expect(args).toContain("HEAD^");
      return git();
    })).toBe("refs/tags/v0.1.77");
    expect(comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "workflow_dispatch" }, {}, git)).toBe("HEAD^");
    expect(() => comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "pull_request" }, {}, git)).toThrow();
    expect(() => comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "schedule" }, {}, git)).toThrow();
    expect(() => comparisonRef({ GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "push" }, { before: "0".repeat(40) }, git)).toThrow();
  });

  it("rejects a committed baseline increase in a clean CI checkout", () => {
    const dir = mkdtempSync(join(tmpdir(), "sr-ratchet-"));
    try {
      const runGit = (...args: string[]) => execFileSync("git", args, { cwd: dir, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
      runGit("init");
      mkdirSync(join(dir, "config/quality"), { recursive: true });
      const path = join(dir, "config/quality/ratchet.json");
      const baseline = (cap: number) => JSON.stringify({ newFileCapLines: cap, frozen: {}, counters: {} });
      writeFileSync(path, baseline(900));
      runGit("add", "config/quality/ratchet.json");
      const env = { ...process.env, GIT_AUTHOR_NAME: "Fixture", GIT_AUTHOR_EMAIL: "fixture@example.test", GIT_COMMITTER_NAME: "Fixture", GIT_COMMITTER_EMAIL: "fixture@example.test" };
      const commit = (parent?: string) => {
        const tree = runGit("write-tree");
        const args = ["commit-tree", tree, ...(parent ? ["-p", parent] : []), "-m", "fixture"];
        const id = execFileSync("git", args, { cwd: dir, env, encoding: "utf8" }).trim();
        runGit("update-ref", "HEAD", id);
        return id;
      };
      const before = commit();
      runGit("update-ref", "refs/tags/v0.1.77", before);
      writeFileSync(path, baseline(999));
      runGit("add", "config/quality/ratchet.json");
      const current = commit(before);
      expect(runGit("status", "--short")).toBe("");
      const eventPath = join(dir, "event.json");
      const verify = (eventName: string, ref: string, event: object) => {
        writeFileSync(eventPath, JSON.stringify(event));
        return spawnSync(process.execPath, [resolve("scripts/verify-ratchet-update.mjs")], {
          cwd: dir, encoding: "utf8", env: { ...env, GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: eventName, GITHUB_REF: ref, GITHUB_EVENT_PATH: eventPath },
        });
      };
      for (const [eventName, ref, event] of [
        ["push", "refs/heads/master", { before }],
        ["pull_request", "refs/pull/1/merge", { pull_request: { base: { sha: before } } }],
        ["push", "refs/tags/v0.1.78", { before: "0".repeat(40) }],
      ] as const) {
        const result = verify(eventName, ref, event);
        expect(result.status, `${eventName} ${ref}: ${result.stderr}`).toBe(1);
        expect(result.stderr).toContain("900 → 999");
      }
      for (const before of ["0".repeat(40), "b".repeat(40), current]) {
        const result = verify("push", "refs/heads/master", { before });
        expect(result.status).toBe(2);
        expect(result.stderr).toContain("检查失败");
      }
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });
});

it("release waits for the reusable complete quality workflow", () => {
  const release = readFileSync(".github/workflows/release.yml", "utf8");
  const gates = readFileSync(".github/workflows/gates.yml", "utf8");
  expect(release).toMatch(/quality:\s+permissions:\s+contents: read\s+uses: \.\/\.github\/workflows\/gates\.yml/);
  expect(release).toMatch(/create-release:\s+needs: quality/);
  expect(release).toMatch(/build-tauri:\s+needs: create-release/);
  expect(release).toContain("needs: [create-release, build-tauri]");
  expect(gates).toContain("workflow_call:");
  expect(gates).toContain("fetch-depth: 0");
  for (const command of ["npm test", "npm run build", "npm run check:forbidden", "npm run ratchet:verify", "cargo check --locked --lib", "cargo clippy --locked --lib --all-targets -- -D warnings", "cargo test --lib"]) {
    expect(gates).toContain(`run: ${command}`);
  }
});
