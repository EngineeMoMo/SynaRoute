import { HardDrive, Lock, Wifi, ScrollText, Trash2, Code2, AlertTriangle } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { useT } from "@/hooks/useLang";

/**
 * 数据与隐私区。
 *
 * 每一条都对应软件的实际行为，不写「绝对安全 / 通过某某认证」这类无法验证的话。
 * 最后那条「需要你注意的风险」是刻意留的：把本地代理监听回环端口、DPAPI 防不了
 * 同账户下的程序这两点讲明白，比含糊其辞更可信。
 *
 * 排布上分两组：**六条事实走三列两行**（正好排满），**风险单独整行**。
 * 之前七条一起丢进三列网格，最后一行只剩风险那一张卡孤零零地挂着，
 * 看起来像是漏排了。加条目时请成三个一组地加，别破坏这个整数。
 */
const facts: { id: string; icon: LucideIcon; i18nPrefix: string }[] = [
  { id: "storage", icon: HardDrive, i18nPrefix: "security.storage" },
  { id: "encryption", icon: Lock, i18nPrefix: "security.encryption" },
  { id: "network", icon: Wifi, i18nPrefix: "security.network" },
  { id: "logs", icon: ScrollText, i18nPrefix: "security.logs" },
  { id: "delete", icon: Trash2, i18nPrefix: "security.delete" },
  { id: "source", icon: Code2, i18nPrefix: "security.source" },
];

export function Security() {
  const t = useT();

  return (
    <Section id="security">
      <SectionTitle title={t("security.title")} subtitle={t("security.subtitle")} />

      <div className="mt-12 grid gap-5 md:grid-cols-2 lg:grid-cols-3">
        {facts.map((item, i) => {
          const Icon = item.icon;
          return (
            <Reveal key={item.id} delay={i * 50} className="h-full">
              <div className="flex h-full flex-col rounded-card border border-border bg-surface p-6 shadow-card">
                <div className="flex items-center gap-3">
                  <span className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-control bg-surface-hover text-text-secondary">
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

      {/* 风险提示：整行 + 图标居左，读起来像一段提示而不是「第七个功能」 */}
      <Reveal delay={320}>
        <div className="mt-5 flex flex-col gap-4 rounded-card border border-warning/40 bg-warning/8 p-6 sm:flex-row sm:items-start sm:gap-5">
          <span className="inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-control bg-warning/12 text-warning">
            <AlertTriangle size={20} aria-hidden="true" />
          </span>
          <div>
            <h3 className="text-base font-semibold text-text-primary">{t("security.risk.title")}</h3>
            <p className="mt-2 max-w-3xl text-sm leading-relaxed text-text-secondary">
              {t("security.risk.desc")}
            </p>
          </div>
        </div>
      </Reveal>
    </Section>
  );
}
