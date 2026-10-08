import type { TFunc } from "./i18n";

export const consultationTemplates = ["general", "debug", "compare", "review"] as const;
export type ConsultationTemplate = (typeof consultationTemplates)[number];

/** Keep the submitted prompt pinned through plan → preview, even after UI changes. */
export function consultationPrompt(input: string, template: ConsultationTemplate, t: TFunc): string {
  if (template === "general") return input.trim();
  return `${t(`brain.template.${template}.instruction`)}\n\n${t("brain.consultQuestion")}\n${input.trim()}`;
}

export type ReportSection = { kind: "conclusion" | "evidence" | "disagreements" | "verification"; text: string };
const headings: Record<string, ReportSection["kind"]> = {
  "推荐结论": "conclusion", "Recommendation": "conclusion",
  "依据与共识": "evidence", "Evidence and agreement": "evidence",
  "分歧与待确认": "disagreements", "Disagreements and unknowns": "disagreements",
  "验证与下一步": "verification", "Verification and next steps": "verification",
};

/** Presentation only: never repair, discard or reinterpret incomplete model output. */
export function parseConsultationReport(content: string): ReportSection[] | null {
  const sections: ReportSection[] = [];
  let current: ReportSection | undefined;
  let fence: { char: string; length: number } | undefined;
  for (const line of content.split(/\r?\n/)) {
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if (marker) {
      if (!fence) fence = { char: marker[1][0], length: marker[1].length };
      else if (marker[1][0] === fence.char && marker[1].length >= fence.length && !marker[2].trim()) fence = undefined;
      if (!current) return null;
      current.text += `${line}\n`;
      continue;
    }
    const heading = !fence && /^##\s+(.+?)\s*#*\s*$/.exec(line);
    const kind = heading ? headings[heading[1]] : undefined;
    if (kind) {
      if (sections.some((s) => s.kind === kind)) return null;
      current = { kind, text: "" };
      sections.push(current);
    } else if (current) current.text += `${line}\n`;
    else if (line.trim()) return null;
  }
  if (fence || sections.length !== 4 || sections.some((s) => !s.text.trim())) return null;
  return sections.map((s) => ({ ...s, text: s.text.trim() }));
}
