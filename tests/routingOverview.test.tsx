import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";
import { RoutingOverviewPage } from "@/pages/RoutingOverviewPage";
import { attemptLabel, connectionReason, createOverviewRefresher, type RoutingOverview } from "@/lib/routingOverview";
import { overviewEn, overviewZh } from "@/lib/i18n.overview";
import { useStore } from "@/store";
import { translate, type TFunc } from "@/lib/i18n";

vi.mock("@/lib/useT", () => ({ useT: (): TFunc => (key, vars) => translate(useStore.getState().lang, key, vars) }));

const state = vi.hoisted(() => ({ data: null as RoutingOverview | null, failed: false, loading: false, refresh: vi.fn() }));
vi.mock("@/lib/useRoutingOverview", () => ({ useRoutingOverview: () => state }));

function fixture(): RoutingOverview {
  return {
    capturedAt: 1_800_000_000_000,
    vaultLocked: false,
    categories: ["claude-cli", "claude-desktop", "codex"].map((categoryId, i) => ({
      categoryId: categoryId as "claude-cli" | "claude-desktop" | "codex",
      running: i !== 1, endpoint: `http://127.0.0.1:${47100 + i}`, keyCount: i === 1 ? 0 : 2, enabledCount: i === 1 ? 0 : 2,
      primaryKey: i === 1 ? null : "Configured primary", configuredModel: "configured-model", configuredEffort: "high",
      connection: { state: i === 2 ? "unknown" as const : "configured" as const, reason: i === 2 ? "invalid_toml" : "matches", clientModel: i === 1 ? null : "client-model" },
      latest: i === 1 ? null : { categoryId: categoryId as "claude-cli" | "codex", keyId: "backup-id", keyName: "Actual backup", requestedModel: "routing-alias", realModel: "actual-model", streaming: true, ok: i === 0, status: i === 0 ? 200 : 429, latencyMs: 128, observedAt: 1_799_999_999_000 },
    })),
  };
}
function render() {
  return renderToStaticMarkup(createElement(RoutingOverviewPage, { onManage: vi.fn(), onOpenLogs: vi.fn(), onOpenReliability: vi.fn() }));
}

beforeEach(() => { state.data = fixture(); state.failed = false; state.loading = false; useStore.setState({ lang: "zh" }); });

describe("routing overview", () => {
  it("renders all clients and distinguishes configured values from observed attempts", () => {
    const html = render();
    expect(html.match(/<article /g)).toHaveLength(3);
    for (const value of ["Configured primary", "Actual backup", "configured-model", "client-model", "routing-alias", "actual-model"]) expect(html).toContain(value);
    expect(html).toContain("流式已建流");
    expect(html).toContain("尝试失败");
    expect(html).toContain("HTTP 429");
    expect(html).toContain("路由模型（覆盖后）");
    expect(html).toContain("配置无法确认");
    expect(html).toContain("Codex TOML 配置无法解析");
    expect(html).toContain("本次运行尚无线路尝试记录");
    expect(html).not.toContain(">已接入<");
  });

  it("never labels established streams as completed responses", () => {
    const attempt = fixture().categories[0].latest!;
    expect(attemptLabel(attempt)).toBe("streamEstablished");
    expect(attemptLabel({ ...attempt, streaming: false })).toBe("responseReceived");
    expect(attemptLabel({ ...attempt, ok: false })).toBe("failed");
  });

  it("shows stale snapshots, empty loading and read failure as different states", () => {
    state.failed = true;
    expect(render()).toContain(overviewZh["overview.stale"]);
    expect(render()).toContain("Actual backup");
    state.data = null;
    expect(render()).toContain(overviewZh["overview.error"]);
    expect(render()).not.toContain("<article");
    state.failed = false; state.loading = true;
    expect(render()).toContain('role="status"');
  });

  it("discloses a locked vault and translates all new presentation", () => {
    state.data!.vaultLocked = true;
    expect(render()).toContain(overviewZh["overview.vaultLocked"]);
    useStore.setState({ lang: "en" });
    const html = render();
    expect(html).toContain("Routing overview");
    expect(html).toContain("Stream established");
    expect(html).toContain(overviewEn["overview.vaultLocked"]);
    expect(html).not.toMatch(/overview\.[a-zA-Z]/);
    expect(connectionReason("future_reason")).toBe("overview.reason.unknown");
  });

  it("keeps unknown and unspecified client models distinct from a session choice", () => {
    state.data!.categories[0].connection.clientModel = null;
    state.data!.categories[2].connection.clientModel = null;
    const html = render();
    expect(html).toContain(overviewZh["overview.clientUnknown"]);
    expect(html).toContain(overviewZh["overview.clientUnset"]);
    expect(html).toContain(overviewZh["overview.clientChoice"]);
  });

  it("keeps backend reason codes translated and does not perform writes or probes", () => {
    const source = readFileSync("src-tauri/src/tools/overview.rs", "utf8");
    const rust = inspectableLines(source, "overview.rs").map((line: { text: string }) => line.text).join("\n");
    const codes = [...rust.matchAll(/"([a-z]+(?:_[a-z]+)*)"/g)].map((m) => m[1]);
    const reasons = [...new Set(codes.filter((code) => overviewZh[`overview.reason.${code}`]))];
    expect(reasons.length).toBeGreaterThanOrEqual(18);
    for (const reason of reasons) {
      expect(connectionReason(reason)).toBe(`overview.reason.${reason}`);
      expect(overviewEn[`overview.reason.${reason}`]).toBeTruthy();
    }
    expect(rust).not.toMatch(/(?:fs::write|File::create|\.write_all|\.set_active_model|\.start|\.stop)\s*\(/);
    expect(rust).not.toContain("reqwest::");
  });

  it("wires the route, command, backend registration and event refresh", () => {
    const app = readFileSync("src/App.tsx", "utf8");
    const sidebar = readFileSync("src/components/Sidebar.tsx", "utf8");
    const bridge = readFileSync("src/lib/bridge.ts", "utf8");
    const rust = readFileSync("src-tauri/src/lib.rs", "utf8");
    const hook = readFileSync("src/lib/useRoutingOverview.ts", "utf8");
    expect(sidebar).toContain('key: "overview"');
    expect(app).toContain('nav === "overview"');
    expect(app).toContain("<RoutingOverviewPage onManage={handleNav}");
    expect(bridge).toContain('call<RoutingOverview>("routing_overview"');
    expect(rust.slice(rust.indexOf("generate_handler!"))).toContain("tools::overview::routing_overview");
    expect(hook).toContain("api.routingOverview()");
    expect(hook).toContain('useBackendEvent(["config", "settings", "health", "proxy", "vault", "logs"]');
    expect(hook).toContain("usePolling(() => void refresh(), FALLBACK_POLL_MS)");
    expect(hook).toContain("createOverviewRefresher(");
    expect(hook).toContain("current.dispose()");
    expect(hook).not.toContain("listAllEvents");
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe("overview refresh lifecycle", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("coalesces bursts without concurrent IPC and keeps one trailing refresh", async () => {
    const first = deferred<RoutingOverview>();
    const read = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValue(fixture());
    const update = vi.fn();
    const loading = vi.fn();
    const runner = createOverviewRefresher(read, update, loading, () => true);
    const pending = runner.refresh();
    for (let i = 0; i < 100; i += 1) runner.schedule();
    await vi.advanceTimersByTimeAsync(500);
    expect(read).toHaveBeenCalledTimes(1);
    runner.schedule();
    first.resolve(fixture());
    await pending;
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(500);
    expect(read).toHaveBeenCalledTimes(2);
    expect(update).toHaveBeenCalledTimes(2);
    expect(vi.getTimerCount()).toBe(0);
    expect(loading.mock.calls).toEqual([[true], [false], [true], [false]]);
    runner.dispose();
  });

  it("does not read after the window becomes hidden and refreshes on return", async () => {
    let visible = true;
    const read = vi.fn().mockResolvedValue(fixture());
    const runner = createOverviewRefresher(read, vi.fn(), vi.fn(), () => visible);
    runner.schedule();
    visible = false;
    await vi.advanceTimersByTimeAsync(500);
    await runner.refresh();
    runner.schedule();
    expect(read).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
    visible = true;
    await runner.refresh();
    expect(read).toHaveBeenCalledTimes(1);
    runner.dispose();
  });

  it("drops old successes and failures after cleanup while a fresh mount can load", async () => {
    for (const failed of [false, true]) {
      const previous = deferred<RoutingOverview>();
      const oldUpdate = vi.fn();
      const oldLoading = vi.fn();
      const oldRead = vi.fn(() => previous.promise);
      const old = createOverviewRefresher(oldRead, oldUpdate, oldLoading, () => true);
      const pending = old.refresh();
      old.schedule();
      old.dispose();
      const nextUpdate = vi.fn();
      const current = createOverviewRefresher(async () => fixture(), nextUpdate, vi.fn(), () => true);
      await current.refresh();
      if (failed) previous.reject(new Error("late")); else previous.resolve(fixture());
      await pending;
      await vi.advanceTimersByTimeAsync(1000);
      expect(oldUpdate).not.toHaveBeenCalled();
      expect(oldLoading.mock.calls).toEqual([[true]]);
      expect(oldRead).toHaveBeenCalledTimes(1);
      expect(nextUpdate).toHaveBeenCalledTimes(1);
      current.dispose();
    }
  });

  it("keeps the last successful snapshot through failure and clears the error on retry", async () => {
    const before = fixture();
    const after = { ...before, capturedAt: before.capturedAt + 1 };
    const read = vi.fn().mockResolvedValueOnce(before).mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(after);
    let last: RoutingOverview | null = null;
    let failed = false;
    const runner = createOverviewRefresher(read, (data, error) => { if (data !== null) last = data; failed = error; }, vi.fn(), () => true);
    await runner.refresh();
    expect(last).toBe(before);
    await runner.refresh();
    expect(last).toBe(before);
    expect(failed).toBe(true);
    await runner.refresh();
    expect(last).toBe(after);
    expect(failed).toBe(false);
    runner.dispose();
  });
});
