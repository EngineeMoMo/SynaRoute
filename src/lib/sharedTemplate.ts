import type { CategoryType, Protocol } from "@/types";
export interface SharedRoute {
  categoryId: CategoryType;
  protocol: Protocol;
  models: { realName: string; contextWindow: number | null; maxOutputTokens: number | null }[];
  mappings: { expectedName: string; realName: string }[];
  defaultModel: string | null;
  tiers: (string | null)[];
  allowNamedModelFallback: boolean;
  temperature: number | null;
  topP: number | null;
  timeoutMs: number | null;
}
export interface SharedTemplate { format: string; version: number; routes: SharedRoute[] }
export interface SharedImportResult { added: number; undoToken: string }
