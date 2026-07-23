import { Badge } from "@/components/ui/Badge";
import type { HealthState } from "@/types";
import { useT } from "@/lib/useT";

/** Key 健康状态徽标（对齐 UI 文档 §5 状态色）——含熔断态派生 */
export function HealthBadge({ health }: { health: HealthState }) {
  const t = useT();
  const remainMs = health.breakerUntil ? health.breakerUntil - Date.now() : 0;
  const breaking = remainMs > 0;

  if (breaking) {
    const remainSec = Math.ceil(remainMs / 1000);
    // 探测/熔断分离后：熔断只由「实时请求连续失败」驱动，健康探测绝不触发熔断。
    // 故熔断来源恒为实时流量，不再区分 fromProbe。
    // breakerUntil 是未来时刻：用绝对钟点表示「熔断至几点几分」，别再套「N 秒前」的相对格式（会显示成负数）。
    const time = new Date(health.breakerUntil!).toLocaleTimeString();
    return (
      <Badge variant="warning" title={t("health.breakerFromLive", { time })}>
        {t("health.breakingRemain", { sec: remainSec })}
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
      // 探测/熔断分离：down 仅表示「健康探测不可达」，不代表被排除路由——只要未熔断，
      // 该 Key 仍会被尝试（真实流量才是可用性的最终裁判）。故用橙色警告而非红色「不可用」，
      // 并在提示里说明它仍会被路由，避免用户以为这个 Key 已彻底停用。
      return (
        <Badge variant="warning" title={t("health.downProbeTitle")}>
          {t("health.downProbe")}
        </Badge>
      );
    case "checking":
      return <Badge variant="info">{t("health.checking")}</Badge>;
    default:
      return <Badge variant="neutral">{t("health.unknown")}</Badge>;
  }
}
