import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ToggleRow } from "../src/components/ToggleRow";
import { MasterPasswordToggle } from "../src/components/MasterPasswordToggle";

describe("ToggleRow accessible relationships", () => {
  it("links each switch to its own title, description and persistent cost", () => {
    const markup = renderToStaticMarkup(createElement("div", null,
      ...["Master password", "LAN access"].map(title => createElement(ToggleRow, {
        key: title, title, desc: "Setting explanation", cost: "Risk explanation",
        checked: false, onChange: () => {},
      })),
    ));
    const labels = [...markup.matchAll(/aria-labelledby="([^"]+)"/g)].map(match => match[1]);
    expect(new Set(labels).size).toBe(2);
    for (const label of labels) expect(markup).toContain('id="' + label + '"');
    const descriptions = [...markup.matchAll(/aria-describedby="([^"]+)"/g)];
    expect(descriptions).toHaveLength(2);
    for (const match of descriptions) {
      expect(match[1].split(" ")).toHaveLength(2);
      for (const id of match[1].split(" ")) expect(markup).toContain('id="' + id + '"');
    }
    expect(markup).toContain('role="switch"');
    expect(markup).toContain('aria-checked="false"');
  });
  it("does not reference an absent cost and preserves disabled state", () => {
    const markup = renderToStaticMarkup(createElement(ToggleRow, {
      title: "Master password", desc: "Required", checked: true, disabled: true, onChange: () => {},
    }));
    expect(markup.match(/aria-describedby="([^"]+)"/)?.[1]).not.toContain(" ");
    expect(markup).toContain('disabled=""');
    expect(markup).toContain('aria-checked="true"');
  });
  it.each([
    [null, true],
    [{ required: true, enabled: false, locked: false }, false],
    [{ required: true, enabled: true, locked: false }, true],
    [{ required: false, enabled: true, locked: false }, false],
  ] as const)("enforces the platform policy in the switch: %j", (state, disabled) => {
    const markup = renderToStaticMarkup(createElement(MasterPasswordToggle, { state, onChange: () => {} }));
    expect(markup.includes('disabled=""')).toBe(disabled);
  });
});
