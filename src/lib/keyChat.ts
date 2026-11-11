import type { CategoryType, Protocol, ProviderKey, TokenUsage } from "@/types";

export interface KeyChatMessage { role: "user" | "assistant"; content: string }
export interface KeyChatRequest {
  keyId: string;
  baseUrl: string;
  protocol: Protocol;
  model: string;
  messages: KeyChatMessage[];
}
export interface KeyChatStarted { requestId: string; categoryId: CategoryType }
export interface KeyChatReply {
  keyId: string;
  model: string;
  protocol: Protocol;
  text: string;
  latencyMs: number;
  usage: TokenUsage | null;
  truncated: boolean;
}

export function keyChatModels(key: ProviderKey): string[] {
  return [...new Set([key.defaultModel, ...key.models.map((model) => model.realName),
    ...key.mappings.map((mapping) => mapping.expectedName)].filter((model): model is string => !!model?.trim()))];
}

export function conversationError(messages: KeyChatMessage[]): string | null {
  if (messages.length > 63) return "keyChat.historyLimit";
  const encoder = new TextEncoder();
  let total = 0;
  for (const message of messages) {
    const bytes = encoder.encode(message.content).length;
    total += bytes;
    if (bytes > 32 * 1024 || total > 128 * 1024) return "keyChat.sizeLimit";
  }
  return null;
}

export class KeyChatRun {
  cancelled = false;
  private started?: KeyChatStarted;
  private stopping?: Promise<unknown>;

  constructor(private readonly stopRequest: (started: KeyChatStarted) => Promise<unknown>) {}

  async onStarted(started: KeyChatStarted) {
    this.started = started;
    if (this.cancelled) await this.stop();
  }

  cancel() {
    this.cancelled = true;
    return this.stop();
  }

  private stop(): Promise<unknown> {
    if (!this.started) return Promise.resolve();
    if (!this.stopping) {
      this.stopping = this.stopRequest(this.started).catch((error) => {
        this.stopping = undefined;
        throw error;
      });
    }
    return this.stopping;
  }
}
