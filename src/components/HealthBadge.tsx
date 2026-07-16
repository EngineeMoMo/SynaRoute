import { Badge } from "@/components/ui/Badge";
import type { HealthState } from "@/types";
import { formatRelativeTime } from "@/lib/utils";
import { useT } from "@/lib/useT";

/** Key 健康状态徽标（对齐 UI 文档 §5 状态色）——含熔断态派生 */
export function HealthBadge({ health }: { health: HealthState }) {
  const t = useT();
  const breaking = health.breakerUntil && health.breakerUntil > Date.now();

  if (breaking) {
    return (
      <Badge variant="warning" title={t("health.breakerUntilTitle", { time: formatRelativeTime(health.breakerUntil) })}>
        {t("health.breaking")}
      </Badge>
    );
  }

  switch (health.status) {
    case "up":
      return (
        <Badge variant="success" title={t("health.latencyTitle", { ms: health.latencyMs ?? "-" })}>
          {t("health.up")}
        </Badge>
      );
    case "down":
      return <Badge variant="danger">{t("health.down")}</Badge>;
    case "checking":
      return <Badge variant="info">{t("health.checking")}</Badge>;
    default:
      return <Badge variant="neutral">{t("health.unknown")}</Badge>;
  }
}
