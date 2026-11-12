import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { conversationError, keyChatModels, KeyChatRun, type KeyChatStarted } from "../src/lib/keyChat";
import { keyChatEn, keyChatZh } from "../src/lib/i18n.keyChat";
import { translate } from "../src/lib/i18n";
import { KeyChatDialog } from "../src/components/KeyChatDialog";
import { Combobox } from "../src/components/ui/Combobox";
import { api } from "../src/lib/bridge";
import type { ProviderKey } from "../src/types";

vi.mock("../src/lib/useT", () => ({ useT: () => (key: string, variables?: Record<string, string | number>) => translate("en", key, variables) }));

const provider: ProviderKey = {
  id: "selected", categoryId: "codex", name: "Test provider", vendor: "custom",
  baseUrl: "https://provider.example/v1", protocol: "openai_chat", hasSecret: true,
  enabled: false, allowInAggregate: false, priority: 0, params: {},
  models: [{ realName: "model-a", source: "manual" }, { realName: "model-b", source: "manual" }],
  mappings: [{ id: "mapping", expectedName: "alias", realName: "model-a" }],
  defaultModel: "model-a", health: { status: "unknown", failCount: 0 },
};
const started: KeyChatStarted = { requestId: "request-id", categoryId: "codex" };

describe("single-Key conversation tests", () => {
  it("deduplicates model suggestions and includes this Key's aliases", () => {
    expect(keyChatModels(provider)).toEqual(["model-a", "model-b", "alias"]);
  });

  it("enforces UTF-8 byte and history limits", () => {
    expect(conversationError([{ role: "user", content: "你好" }])).toBeNull();
    expect(conversationError([{ role: "user", content: "文".repeat(12_000) }])).toBe("keyChat.sizeLimit");
    expect(conversationError(Array.from({ length: 9 }, () => ({ role: "user" as const, content: "a".repeat(16_000) })))).toBe("keyChat.sizeLimit");
    expect(conversationError(Array.from({ length: 65 }, () => ({ role: "user" as const, content: "test" })))).toBe("keyChat.historyLimit");
  });

  it("does not lose stop requests made before the backend acknowledges a start", async () => {
    const stop = vi.fn(async () => "requested");
    const run = new KeyChatRun(stop);
    await run.cancel();
    expect(stop).not.toHaveBeenCalled();
    await run.onStarted(started);
    expect(stop).toHaveBeenCalledExactlyOnceWith(started);
    expect(run.cancelled).toBe(true);
  });

  it("deduplicates stop requests and keeps independent runs isolated", async () => {
    const stop = vi.fn(async () => "requested");
    const oldRun = new KeyChatRun(stop);
    const newRun = new KeyChatRun(stop);
    await oldRun.onStarted(started);
    await Promise.all([oldRun.cancel(), oldRun.cancel()]);
    expect(stop).toHaveBeenCalledTimes(1);
    expect(newRun.cancelled).toBe(false);
  });

  it("allows retrying an unsuccessful stop IPC", async () => {
    const stop = vi.fn().mockRejectedValueOnce(new Error("IPC unavailable")).mockResolvedValue("requested");
    const run = new KeyChatRun(stop);
    await run.onStarted(started);
    await expect(run.cancel()).rejects.toThrow("IPC unavailable");
    await run.cancel();
    expect(stop).toHaveBeenCalledTimes(2);
  });

  it("shows the fixed provider, model entry and real-request privacy warnings", () => {
    const html = renderToStaticMarkup(createElement(KeyChatDialog, { provider, onClose: () => {} }));
    expect(html).toContain("Single-Key conversation test");
    expect(html).toContain("Test provider");
    expect(html).toContain("https://provider.example/v1");
    expect(html).toContain("without failover");
    expect(html).toContain("may incur charges");
    expect(html).toContain("not saved locally");
    expect(html).toContain('role="log"');
    expect(html).toContain("Clear conversation");
    expect(html).toContain("Send test");
    const modelInput = html.match(/<input\b[^>]*>/)?.[0] ?? "";
    const modelId = modelInput.match(/\bid="([^"]+)"/)?.[1];
    expect(modelId).toBeTruthy();
    expect(html).toContain(`for="${modelId}"`);
    expect(modelInput).toContain('maxLength="200"');
    expect(modelInput).toContain('value="model-a"');
    expect(html).not.toMatch(/keyChat\.[A-Za-z]/);
  });

  it.each([false, true])("locks both model entry and its picker while busy=%s", (disabled) => {
    const html = renderToStaticMarkup(createElement(Combobox, {
      value: "model-b", options: keyChatModels(provider), onChange: vi.fn(),
      emptyHint: "Enter a model", disabled,
    }));
    const input = html.match(/<input\b[^>]*>/)?.[0] ?? "";
    const picker = html.match(/<button\b[^>]*>/)?.[0] ?? "";
    expect(input).toContain('value="model-b"');
    expect(picker).toContain('type="button"');
    expect(input.includes('disabled=""')).toBe(disabled);
    expect(picker.includes('disabled=""')).toBe(disabled);
  });

  it("explains missing credentials without offering a fake successful test", async () => {
    const html = renderToStaticMarkup(createElement(KeyChatDialog, { provider: { ...provider, hasSecret: false }, onClose: () => {} }));
    expect(html).toContain("Configure this Key");
    await expect(api.testKeyConversation({ keyId: provider.id, baseUrl: provider.baseUrl, protocol: provider.protocol, model: "model-a", messages: [{ role: "user", content: "test" }] }, () => {})).rejects.toThrow("desktop app");
  });

  it("provides every conversation label in both languages", () => {
    expect(Object.keys(keyChatZh).sort()).toEqual(Object.keys(keyChatEn).sort());
    for (const language of ["zh", "en"] as const) {
      for (const key of Object.keys(keyChatZh)) {
        expect(translate(language, key, { model: "model-a", ms: 10, input: 3, output: 4 })).not.toBe(key);
      }
    }
  });
});
