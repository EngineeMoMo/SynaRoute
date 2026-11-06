import { describe, expect, it } from "vitest";
import { defaultRoutingPolicy, groupModelId, parseSensitiveTerms, reorderRule, routingChoices, sceneMatches, type RoutingCategoryView } from "../src/lib/routingPolicies";
import { readFileSync } from "node:fs";
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

describe("routing policies", () => {
  it("defaults to off and only advertises enabled groups", () => {
    const policy = defaultRoutingPolicy();
    expect(policy.affinity).toBe("off"); expect(policy.privacy.mode).toBe("off");
    const group = { id: "one", name: "Work", enabled: true, defaultModel: "real", judge: null, rules: [] };
    const row: RoutingCategoryView = { categoryId: "codex", policy: { ...policy, groups: [group, { ...group, id: "two", enabled: false }] }, models: ["real"], keys: [], current: { categoryId: "codex", primaryKeyId: "key", model: "real", effort: "high" }, issues: [], activity: null };
    expect(routingChoices(row).map((o) => o.value)).toEqual(["real", groupModelId(group)]);
    expect(sceneMatches({ id: "s", name: "S", routes: [{ ...row.current, primaryKeyId: null }] }, [row])).toBe(true);
    expect(sceneMatches({ id: "s", name: "S", routes: [{ ...row.current, model: null }] }, [row])).toBe(false);
    expect(sceneMatches({ id: "s", name: "S", routes: [] }, [row])).toBe(false);
  });
  it("normalizes terms and reorders without mutating the draft", () => {
    expect(parseSensitiveTerms(" alpha \r\n\nalpha\n中文词条")).toEqual(["alpha", "中文词条"]);
    const rules = [{ id: "a", intent: "A", model: "x" }, { id: "b", intent: "B", model: "y" }];
    expect(reorderRule(rules, 0, 1).map((r) => r.id)).toEqual(["b", "a"]);
    expect(rules[0].id).toBe("a"); expect(reorderRule(rules, 0, -1)).toBe(rules);
  });
  it("registers every frontend command and keeps management reachable", () => {
    const bridge = read("src/lib/bridge.ts"); const registered = read("src-tauri/src/lib.rs");
    for (const command of ["routing_policies", "save_routing_policy", "save_routing_scene", "delete_routing_scene", "preview_routing_scene", "apply_routing_scene", "sync_routing_clients", "clear_routing_sessions", "preview_sensitive_content"]) {
      expect(bridge).toContain(`"${command}"`); expect(registered).toContain(command);
    }
    expect(read("src/App.tsx")).toContain('nav === "policies"');
    expect(read("src/components/Sidebar.tsx")).toContain('key: "policies"');
    expect(read("src/components/CommandPalette.tsx")).toContain("policies:");
  });
  it("preserves null terms, confirms writes, and binds scan lifetime to category", () => {
    const page = read("src/pages/RoutingPoliciesPage.tsx");
    expect(page).toContain("terms === null ? null : parseSensitiveTerms(terms)");
    expect(page).toContain("draft?.revision ?? data.revision");
    expect(page).toContain("SensitiveContentPanel key={category}");
    const scenes = read("src/components/SceneProfilesPanel.tsx");
    expect(scenes).toContain("api.previewRoutingScene(scene.id)");
    expect(scenes).toContain("api.applyRoutingScene(token)");
    expect(scenes).toContain("<ConfirmDialog");
  });
});
