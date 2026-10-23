// The local Laya panel may download and install Python, PyTorch and model weights, and saving its
// endpoint overwrites the category's decision service. Both must only happen after the confirmation
// dialog. The one-click "use this local service" button once called `laya_local_start`; if the service
// died between two polls that click started a full install with no confirmation.
// Source-level because this repo has no DOM test environment.
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { isLoopbackEndpoint } from "../src/lib/decision";

const read = (p: string) => readFileSync(resolve(process.cwd(), p), "utf8");
/** Strip comments: the criterion is about code, not about prose that mentions the commands. */
const codeOnly = (src: string) =>
  src.replace(/\/\*[\s\S]*?\*\//g, "").split(/\r?\n/).map((l) => (l.includes("//") ? l.slice(0, l.indexOf("//")) : l)).join("\n");
const between = (src: string, from: string, to: string) => {
  const start = src.indexOf(from);
  expect(start, `${from} not found`).toBeGreaterThanOrEqual(0);
  return src.slice(start, src.indexOf(to, start));
};
const lineOf = (src: string, at: number) => src.slice(src.lastIndexOf("\n", at) + 1, src.indexOf("\n", at));

const panel = codeOnly(read("src/components/LayaLocalPanel.tsx"));
const decisionPanel = codeOnly(read("src/components/DecisionPanel.tsx"));
const bridge = codeOnly(read("src/lib/bridge.ts"));

describe("Laya local setup requires confirmation", () => {
  it("setup and replacement only start from the confirmation dialog", () => {
    const calls = [...panel.matchAll(/(?<!function )\baction\(([^)]*)\)/g)];
    expect(calls.map((c) => c[1]).sort()).toEqual(['"stop"', "kind", "kind"]);
    for (const call of calls.filter((c) => c[1] === "kind")) {
      const line = lineOf(panel, call.index!);
      // Either the dialog's confirm handler, or the direct path that can only carry launch/use with nothing to replace.
      const viaDialog = line.includes("onConfirm=");
      const direct = line.includes('if ((kind === "launch" || kind === "use") && !replaces) { void action(kind); return; }');
      expect(viaDialog || direct, `action(kind) outside the two allowed places: ${line.trim()}`).toBe(true);
    }
  });

  it("the use button only reads status", () => {
    const useButton = panel.split("\n").find((l) => l.includes("使用此本地服务"));
    expect(useButton).toBeDefined();
    expect(useButton).toContain('request("use")');
    const body = between(panel, "async function action(", "async function setAutostart(");
    expect(body).toContain("api.layaLocalStatus()");
    expect(body.match(/api\.layaLocalStart\(/g)).toHaveLength(1);
  });

  it("the dialog-free launch can never install", () => {
    // The main button skips the dialog for an installed environment; only noInstall keeps that safe.
    expect(panel).toContain('request(canLaunch ? "launch" : "start")');
    expect(panel).toContain('api.layaLocalStart({ repair: kind === "repair", noInstall: kind === "launch" })');
    // Named fields end to end: positional booleans could be swapped at a call site without any error.
    expect(bridge).toContain('call<LayaLocalStatus>("laya_local_start", { repair: opts.repair, noInstall: opts.noInstall }');
    // Tauri maps camelCase arguments to snake_case parameters. A renamed Rust parameter would arrive
    // as None (= false) and the script would install without consent, with no error anywhere.
    const rust = read("src-tauri/src/decision/laya_local.rs");
    expect(rust).toMatch(/pub async fn laya_local_start\([^)]*\bno_install: Option<bool>/);
    expect(rust).not.toMatch(/rename_all\s*=\s*"snake_case"/);
  });

  it("replacing a remote decision service or a stored key is confirmed first", () => {
    expect(decisionPanel).toContain("replaces={replacesRemote}");
    expect(decisionPanel).toMatch(/const replacesRemote = \(hasSecret && !clearSecret\) \|\| \(!!config\.endpoint\.trim\(\) && !isLoopbackEndpoint\(config\.endpoint\)\)/);
    for (const url of ["http://127.0.0.1:8000/v1/systemone", "http://localhost:8000/x", "http://[::1]:8000/x", "http://foo.localhost/x"]) {
      expect(isLoopbackEndpoint(url), url).toBe(true);
    }
    // A false negative only adds a confirmation; a false positive would overwrite without one.
    for (const url of ["https://api.typesafe.ai/v1/systemone", "http://192.168.1.5:8000/x", "http://localhost.example.com/x", "", "not a url"]) {
      expect(isLoopbackEndpoint(url), url).toBe(false);
    }
  });

  it("switches only record preferences", () => {
    // Neither may start, stop or install anything now; they apply at the next launch or download.
    const autostart = between(panel, "async function setAutostart(", "async function setMirror(");
    const mirror = between(panel, "async function setMirror(", "async function copyDiagnostics(");
    expect(autostart).toContain("api.layaLocalSetAutostart(enabled)");
    expect(mirror).toContain("api.layaLocalSetMirror(enabled)");
    for (const body of [autostart, mirror]) {
      expect(body).not.toContain("layaLocalStart");
      expect(body).not.toMatch(/\baction\(/);
    }
    expect(panel).toContain("checked={!!state?.autostart}");
    expect(panel).toContain("checked={!!state?.mirror}");
  });

  it("a stop waiting for the Python installer blocks every other action", () => {
    // Not "working" during "stopping" would re-enable the install button while the installer still runs.
    expect(panel).toMatch(/const activePhases = \[[^\]]*"stopping"/);
    expect(panel).toContain("disabled={!state || busy || working}");
    expect(panel).toContain('disabled={busy || state?.phase === "stopping"}');
    expect(panel).toMatch(/stopping: text\(/);
  });

  it("other categories are only moved to the endpoint just saved", () => {
    const body = between(decisionPanel, "async function configureLocal(", "const replacesRemote");
    expect(body).toContain("api.layaLocalSyncEndpoints(endpoint)");
    expect(decisionPanel.match(/layaLocalSyncEndpoints\(/g)).toHaveLength(1);
  });

  it("every command the panel uses is defined, bridged and registered", () => {
    const rust = read("src-tauri/src/decision/laya_local.rs");
    const lib = read("src-tauri/src/lib.rs");
    const commands: Record<string, [string, string]> = {
      laya_local_status: ["layaLocalStatus", panel],
      laya_local_start: ["layaLocalStart", panel],
      laya_local_stop: ["layaLocalStop", panel],
      laya_local_set_autostart: ["layaLocalSetAutostart", panel],
      laya_local_set_mirror: ["layaLocalSetMirror", panel],
      laya_local_sync_endpoints: ["layaLocalSyncEndpoints", decisionPanel],
    };
    for (const [cmd, [method, user]] of Object.entries(commands)) {
      expect(bridge, `${cmd} is not bridged`).toMatch(new RegExp(`${method}: \\([^)]*\\) => call<[^>]+>\\("${cmd}"`));
      expect(user, `${method} is not used`).toContain(`api.${method}(`);
      expect(rust).toMatch(new RegExp(`pub (async )?fn ${cmd}\\(`));
      expect(lib).toContain(`store::decision::laya_local::${cmd}`);
    }
    expect(panel, "the panel must go through the bridge").not.toContain("@tauri-apps/api/core");
  });
});
