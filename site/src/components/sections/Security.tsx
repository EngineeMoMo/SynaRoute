import { HardDrive, Lock, Wifi, ScrollText, Trash2, Code2, AlertTriangle } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { useT } from "@/hooks/useLang";
import { cn } from "@/lib/utils";

/**
 * 数据与隐私区。
 *
 * 每一条都对应软件的实际行为，不写「绝对安全 / 通过某某认证」这类无法验证的话。
 * 最后一条「需要你注意的风险」是刻意留的：把本地代理监听回环端口、DPAPI 防不了
 * 同账户下的程序这两点讲明白，比含糊其辞更可信。
 */
const items: { id: string; icon: LucideIcon; i18nPrefix: string; tone?: "warning" }[] = [
  { id: "storage", icon: HardDrive, i18nPrefix: "security.storage" },
  { id: "encryption", icon: Lock, i18nPrefix: "security.encryption" },
  { id: "network", icon: Wifi, i18nPrefix: "security.network" },
  { id: "logs", icon: ScrollText, i18nPrefix: "security.logs" },
  { id: "delete", icon: Trash2, i18nPrefix: "security.delete" },
  { id: "source", icon: Code2, i18nPrefix: "security.source" },
  { id: "risk", icon: AlertTriangle, i18nPrefix: "security.risk", tone: "warning" },
];

export function Security() {
  const t = useT();

  return (
    <Section id="security">
      <SectionTitle title={t("security.title")} subtitle={t("security.subtitle")} />

      <div className="mt-12 grid gap-5 md:grid-cols-2 lg:grid-cols-3">
        {items.map((item, i) => {
          const Icon = item.icon;
          const warn = item.tone === "warning";
          return (
            <Reveal key={item.id} delay={i * 50} className="h-full">
              <div
                className={cn(
                  "flex h-full flex-col rounded-card border p-6 shadow-card",
                  warn ? "border-warning/40 bg-warning/8" : "border-border bg-surface"
                )}
              >
                <div className="flex items-center gap-3">
                  <span
                    className={cn(
                      "inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-control",
                      warn ? "bg-warning/12 text-warning" : "bg-surface-hover text-text-secondary"
                    )}
                  >
                    <Icon size={18} aria-hidden="true" />
                  </span>
                  <h3 className="text-base font-semibold text-text-primary">{t(`${item.i18nPrefix}.title`)}</h3>
                </div>
                <p className="mt-3 text-sm leading-relaxed text-text-secondary">{t(`${item.i18nPrefix}.desc`)}</p>
              </div>
            </Reveal>
          );
        })}
      </div>
    </Section>
  );
}
