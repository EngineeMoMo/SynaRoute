import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SessionMaintenanceProgress } from "@/components/SessionMaintenanceProgress";
import { SessionIdRepairPanel } from "@/components/SessionIdRepairPanel";
import { sessionsEn, sessionsZh } from "@/lib/i18n.sessions";

describe("Session maintenance accessibility", () => {
  it("reports a real phase and count without manufacturing a percentage", () => {
    const markup = renderToStaticMarkup(createElement(SessionMaintenanceProgress, { value: { phase: "validating", completed: 2, total: 5 } }));
    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain("2 / 5");
    expect(markup).not.toContain("%");
  });

  it("starts with a scan-only action and blocks it while another operation is active", () => {
    const markup = renderToStaticMarkup(createElement(SessionIdRepairPanel, { disabled: true, onBusyChange: () => {}, onDone: async () => {} }));
    expect(markup).toContain('aria-labelledby="session-id-repair-title"');
    expect(markup).toContain('id="session-id-repair-title"');
    expect(markup).toContain('disabled=""');
    expect(markup.match(/<button/g)).toHaveLength(1);
    expect(markup).not.toContain('role="dialog"');
  });

  it("has matching translations for every maintenance state", () => {
    for (const key of Object.keys(sessionsZh).filter((entry) => entry.startsWith("sessions.repair.") || entry.startsWith("sessions.phase."))) {
      expect(sessionsEn[key]).toBeTruthy();
      expect(sessionsEn[key].match(/\{\w+\}/g)?.sort()).toEqual(sessionsZh[key].match(/\{\w+\}/g)?.sort());
    }
  });
});
