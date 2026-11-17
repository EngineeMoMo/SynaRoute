import { describe, expect, it, vi } from "vitest";
import { createVisibleEventGate } from "./visibleEvents";

describe("hidden window events", () => {
  it("coalesces background updates into one refresh on return", () => {
    let visible = false;
    const refresh = vi.fn();
    const gate = createVisibleEventGate(refresh, () => visible);
    for (let index = 0; index < 100; index++) gate.notify();
    gate.resume();
    expect(refresh).not.toHaveBeenCalled();
    visible = true;
    gate.resume();
    gate.resume();
    expect(refresh).toHaveBeenCalledTimes(1);
    gate.notify();
    expect(refresh).toHaveBeenCalledTimes(2);
  });
  it("allows control events while hidden", () => {
    const refresh = vi.fn();
    createVisibleEventGate(refresh, () => false).notify(true);
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
