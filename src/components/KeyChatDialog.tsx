import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Loader2, MessageSquare, Send, Square, X } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { DialogBody, DialogFooter, DialogFrame, DialogHeader } from "@/components/ui/DialogFrame";
import { Tooltip } from "@/components/ui/Tooltip";
import { api } from "@/lib/bridge";
import { conversationError, keyChatModels, KeyChatRun, type KeyChatMessage, type KeyChatReply } from "@/lib/keyChat";
import { useT } from "@/lib/useT";
import { useStore } from "@/store";
import { protocolLabel, type ProviderKey } from "@/types";

const inputClass = "w-full rounded-control border border-border bg-surface-elevated px-3 py-2 text-sm text-text-primary outline-none focus:border-primary disabled:opacity-50";

export function KeyChatButton({ provider }: { provider: ProviderKey }) {
  const [open, setOpen] = useState(false);
  const close = useCallback(() => setOpen(false), []);
  const translate = useT();
  return <>
    <Tooltip content={translate("keyChat.title")} side="top">
      <Button size="icon" variant="ghost" aria-label={translate("keyChat.title")} onClick={() => setOpen(true)}>
        <MessageSquare size={14} />
      </Button>
    </Tooltip>
    {open && createPortal(<KeyChatDialog key={`${provider.id}:${provider.baseUrl}:${provider.protocol}`} provider={provider} onClose={close} />, document.body)}
  </>;
}

export function KeyChatDialog({ provider, onClose }: { provider: ProviderKey; onClose: () => void }) {
  const translate = useT();
  const showToast = useStore((state) => state.showToast);
  const models = keyChatModels(provider);
  const [model, setModel] = useState(models[0] ?? "");
  const [messages, setMessages] = useState<KeyChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [pendingText, setPendingText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reply, setReply] = useState<KeyChatReply | null>(null);
  const active = useRef<KeyChatRun | null>(null);
  const mounted = useRef(true);
  const panel = useRef<HTMLDivElement>(null);
  const transcript = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const modelId = useId();

  const stopFailure = useCallback((reason: unknown) => {
    showToast("error", String((reason as Error)?.message ?? reason));
  }, [showToast]);

  useEffect(() => {
    mounted.current = true;
    const previousFocus = document.activeElement as HTMLElement | null;
    composer.current?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onClose();
      }
      if (event.key !== "Tab") return;
      const controls = Array.from(panel.current?.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled]), textarea:not([disabled]), [tabindex='0']") ?? []);
      const first = controls[0];
      const last = controls[controls.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    };
    window.addEventListener("keydown", keydown, true);
    return () => {
      mounted.current = false;
      void active.current?.cancel().catch(stopFailure);
      window.removeEventListener("keydown", keydown, true);
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, [onClose, stopFailure]);

  useEffect(() => {
    if (transcript.current) transcript.current.scrollTop = transcript.current.scrollHeight;
  }, [messages, pendingText]);

  const clear = () => {
    if (active.current) return;
    setMessages([]); setDraft(""); setReply(null); setError(null);
  };

  const submit = async () => {
    if (active.current || !draft.trim() || !model.trim() || !provider.hasSecret) return;
    const history: KeyChatMessage[] = [...messages, { role: "user", content: draft.trim() }];
    const invalid = conversationError(history);
    if (invalid) { setError(translate(invalid)); return; }
    const run = new KeyChatRun((started) => api.stopActiveRequest(started.categoryId, started.requestId));
    active.current = run;
    setBusy(true); setStopping(false); setError(null); setPendingText(draft.trim());
    try {
      const result = await api.testKeyConversation({
        keyId: provider.id, baseUrl: provider.baseUrl, protocol: provider.protocol,
        model: model.trim(), messages: history,
      }, (started) => { void run.onStarted(started).catch(stopFailure); });
      if (!mounted.current || active.current !== run || run.cancelled) return;
      setMessages([...history, { role: "assistant", content: result.text }]);
      setReply(result); setDraft("");
    } catch (reason) {
      if (mounted.current && active.current === run) {
        setError(run.cancelled ? translate("keyChat.stopped") : String((reason as Error)?.message ?? reason));
      }
    } finally {
      if (active.current === run) {
        active.current = null;
        if (mounted.current) {
          setBusy(false); setStopping(false); setPendingText(null);
          requestAnimationFrame(() => composer.current?.focus());
        }
      }
    }
  };

  const stop = () => {
    const run = active.current;
    if (!run) return;
    setStopping(true);
    void run.cancel().catch((reason) => {
      stopFailure(reason);
      if (mounted.current) setStopping(false);
    });
  };

  return <DialogFrame size="xl" ariaLabel={translate("keyChat.title")} panelRef={panel}
    onBackdropMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <DialogHeader>
      <div className="min-w-0">
        <h2 className="flex items-center gap-2 text-sm font-semibold text-text-primary"><MessageSquare size={17} />{translate("keyChat.title")}</h2>
        <p className="mt-1 truncate text-xs text-text-secondary">{provider.name} · {protocolLabel(provider.protocol)}</p>
      </div>
      <Button size="icon" variant="ghost" aria-label={translate("common.close")} onClick={onClose}><X size={16} /></Button>
    </DialogHeader>
    <DialogBody className="space-y-3">
      <p className="text-xs leading-relaxed text-text-secondary">{translate("keyChat.scope")}</p>
      <p className="break-all font-mono text-[11px] text-text-muted">{provider.baseUrl}</p>
      <label className="block space-y-1 text-xs text-text-secondary" htmlFor={modelId}>
        <span>{translate("keyChat.model")}</span>
        <input id={modelId} list={`${modelId}-options`} className={inputClass} value={model} maxLength={200} disabled={busy}
          placeholder={translate("keyChat.modelPlaceholder")} onChange={(event) => { clear(); setModel(event.target.value); }} />
        <datalist id={`${modelId}-options`}>{models.map((option) => <option key={option} value={option} />)}</datalist>
      </label>
      {!provider.hasSecret && <p role="alert" className="text-xs text-warning">{translate("keyChat.missingSecret")}</p>}
      <div ref={transcript} role="log" aria-label={translate("keyChat.transcript")} aria-live="polite" className="h-64 space-y-3 overflow-y-auto rounded-control border border-border bg-surface-elevated p-3">
        {messages.length === 0 && !pendingText && <p className="text-xs text-text-muted">{translate("keyChat.empty")}</p>}
        {messages.map((message, index) => <div key={index} className="space-y-1">
          <div className={`text-[11px] font-semibold ${message.role === "user" ? "text-primary" : "text-text-secondary"}`}>{translate(message.role === "user" ? "keyChat.you" : "keyChat.assistant")}</div>
          <div className="whitespace-pre-wrap break-words text-sm text-text-primary">{message.content}</div>
        </div>)}
        {pendingText && <div className="whitespace-pre-wrap break-words text-sm text-text-secondary">{pendingText}</div>}
        {busy && <p role="status" className="flex items-center gap-2 text-xs text-text-muted"><Loader2 size={13} className="animate-spin" />{translate(stopping ? "active.stopping" : "keyChat.waiting")}</p>}
      </div>
      {reply && <p className="text-[11px] text-text-secondary">
        {translate("keyChat.result", { model: reply.model, ms: reply.latencyMs })}
        {reply.usage ? ` · ${translate("keyChat.usage", { input: reply.usage.input + (reply.usage.cacheRead ?? 0) + (reply.usage.cacheCreation ?? 0), output: reply.usage.output })}` : ` · ${translate("keyChat.noUsage")}`}
        {reply.truncated && <span className="ml-2 text-warning">{translate("keyChat.truncated")}</span>}
      </p>}
      {error && <p role="alert" className="whitespace-pre-wrap break-words text-xs text-danger">{error}</p>}
      <textarea ref={composer} aria-label={translate("keyChat.prompt")} placeholder={translate("keyChat.prompt")}
        className={`${inputClass} min-h-24 resize-y`} value={draft} maxLength={8000} disabled={busy}
        onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) {
            event.preventDefault(); void submit();
          }
        }} />
      <p className="text-[11px] text-text-muted">{translate("keyChat.billing")}</p>
    </DialogBody>
    <DialogFooter>
      <Button size="sm" variant="ghost" disabled={busy || (!messages.length && !draft)} onClick={clear}>{translate("keyChat.clear")}</Button>
      {busy ? <Button size="sm" variant="danger-outline" disabled={stopping} onClick={stop}><Square size={13} />{translate("active.stop")}</Button>
        : <Button size="sm" disabled={!draft.trim() || !model.trim() || !provider.hasSecret} onClick={() => void submit()}><Send size={13} />{translate("keyChat.send")}</Button>}
    </DialogFooter>
  </DialogFrame>;
}
