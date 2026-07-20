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
    // 熔断来源：探测失败会置 status=down；实时请求失败只动熔断态、不改 status（保持 up/unknown）。
    // 据此区分提示，避免用户对着「健康探测刚成功」的延迟数字误判为探测出的熔断。
    const fromProbe = health.status === "down";
    // breakerUntil 是未来时刻：用绝对钟点表示「熔断至几点几分」，别再套「N 秒前」的相对格式（会显示成负数）。
    const time = new Date(health.breakerUntil!).toLocaleTimeString();
    const title = fromProbe
      ? t("health.breakerFromProbe", { time })
      : t("health.breakerFromLive", { time });
    return (
      <Badge variant="warning" title={title}>
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
      return <Badge variant="danger">{t("health.down")}</Badge>;
    case "checking":
      return <Badge variant="info">{t("health.checking")}</Badge>;
    default:
      return <Badge variant="neutral">{t("health.unknown")}</Badge>;
  }
}
