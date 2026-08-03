import { Badge } from "@/components/ui/Badge";
import type { HealthState } from "@/types";
import { useT } from "@/lib/useT";

/**
 * 探测结论的「保鲜期」。超过这个时长的**失败**结论不再当成当下事实。
 *
 * 取 1 小时：比定时探测的最长间隔宽裕（用户可把间隔设到 30 分钟），
 * 正常启用的 Key 不会被误判成过期；而停用 Key 的状态本就不再更新，
 * 一小时后它是什么样谁也说不准，如实标成「已过期」比给个确定结论诚实。
 */
const STALE_MS = 60 * 60 * 1000;

/**
 * 把毫秒时长拆成 {单位, 数值}，交给 i18n 组装文案。
 *
 * 不在这里直接拼中文串：本项目双语（zh/en 各一份文案），组件里硬编码会让英文界面
 * 出现中文单位。故只算数值 + 选一个 key，措辞交给语言包。
 */
function agoParts(ms: number): { key: string; n: number } {
  const min = Math.floor(ms / 60000);
  if (min < 60) return { key: "health.agoMinutes", n: min };
  const h = Math.floor(min / 60);
  if (h < 24) return { key: "health.agoHours", n: h };
  return { key: "health.agoDays", n: Math.floor(h / 24) };
}

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
    case "down": {
      // 陈旧结论不当成当下事实（2026-08-02 真机发现）：定时探测只扫**启用**的 Key，
      // 一条 Key 停用期间状态会永久冻结在它上次被探测的结论上。实测见过卡片显示
      // 「探测不可达 · 10 天前」，而那家上游早已恢复、真实转发也成功 —— 用户据此
      // 以为「这个 Key 现在是坏的」，方向完全错了。
      //
      // 超过 STALE_MS 就只说「状态已过期」并提示手动检测，不再给出确定性结论。
      // 注意：**只对 down 做这件事**。up 过期不必特别处理（乐观陈旧不会误导用户
      // 去排查一个不存在的故障），而 down 过期会。
      const age = health.lastChecked ? Date.now() - health.lastChecked : 0;
      if (health.lastChecked && age > STALE_MS) {
        const { key, n } = agoParts(age);
        return (
          <Badge
            variant="neutral"
            title={t("health.staleTitle", { ago: t(key, { n: String(n) }) })}
          >
            {t("health.stale")}
          </Badge>
        );
      }
      // 探测/熔断分离：down 仅表示「健康探测不可达」，不代表被排除路由——只要未熔断，
      // 该 Key 仍会被尝试（真实流量才是可用性的最终裁判）。故用橙色警告而非红色「不可用」，
      // 并在提示里说明它仍会被路由，避免用户以为这个 Key 已彻底停用。
      return (
        <Badge variant="warning" title={t("health.downProbeTitle")}>
          {t("health.downProbe")}
        </Badge>
      );
    }
    case "checking":
      return <Badge variant="info">{t("health.checking")}</Badge>;
    default:
      return <Badge variant="neutral">{t("health.unknown")}</Badge>;
  }
}
