import * as React from "react";
import { Card } from "@/components/ui/Card";
import { Badge } from "@/components/ui/Badge";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { HealthBadge } from "@/components/HealthBadge";
import { BrandIcon } from "@/components/BrandIcon";
import { type ProviderKey, protocolLabel } from "@/types";
import { useStore } from "@/store";
import { formatRelativeTime } from "@/lib/utils";
import { useT } from "@/lib/useT";
import { ChevronUp, ChevronDown, RefreshCw, Pencil, Trash2, ArrowRight } from "lucide-react";

/** 单个厂商 Key 卡片（FR-001/003/006/010/011） */
export function KeyCard({ k, onEdit, isFirst, isLast }: {
  k: ProviderKey;
  onEdit: (k: ProviderKey) => void;
  isFirst: boolean;
  isLast: boolean;
}) {
  const { toggleKey, deleteKey, checkHealth, moveKey, setPrimaryKey, vendors } = useStore();
  const t = useT();
  const vendorIcon = vendors.find((v) => v.id === k.vendor)?.icon;
  // 就地二次确认：不用原生 confirm()（在 Tauri WebView2 里行为不可靠，会导致删除不触发）
  const [confirmingDelete, setConfirmingDelete] = React.useState(false);

  return (
    <Card className="p-0">
      <div className="flex items-start gap-3 p-4">
        {/* 优先级上移/下移（FR-010）：越靠上越优先，故障转移先用它。
            拖拽在 Tauri WebView 里不稳，改用明确的上/下按钮，点一下与相邻 Key 交换。 */}
        <div className="mt-0.5 flex flex-col">
          <button
            className="text-text-muted hover:text-text-secondary disabled:cursor-not-allowed disabled:opacity-30"
            title={t("key.moveUp")}
            aria-label={t("key.moveUp")}
            disabled={isFirst}
            onClick={() => void moveKey(k.id, "up")}
          >
            <ChevronUp size={15} />
          </button>
          <button
            className="text-text-muted hover:text-text-secondary disabled:cursor-not-allowed disabled:opacity-30"
            title={t("key.moveDown")}
            aria-label={t("key.moveDown")}
            disabled={isLast}
            onClick={() => void moveKey(k.id, "down")}
          >
            <ChevronDown size={15} />
          </button>
        </div>

        <BrandIcon hint={k.vendor} fallbackLabel={k.name} iconUrl={vendorIcon} size={28} className="mt-0.5" />

        <div className="min-w-0 flex-1">
          {/* 标题行 */}
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-semibold text-text-primary">
              {k.name}
            </span>
            {k.priority === 0 ? (
              <Badge variant="primary">{t("key.primary")}</Badge>
            ) : (
              <button
                type="button"
                onClick={() => void setPrimaryKey(k.id)}
                className="shrink-0 rounded-control border border-border px-1.5 py-0.5 text-[11px] text-text-muted hover:border-primary hover:text-primary"
                title={t("key.setPrimaryHint")}
              >
                {t("key.setPrimary")}
              </button>
            )}
            <HealthBadge health={k.health} />
          </div>

          {/* 端点 */}
          <div className="mt-1 truncate font-mono text-xs text-text-secondary">
            {k.baseUrl}
            <span className="ml-2 text-text-muted">
              · {protocolLabel(k.protocol)} {t("key.protocolSuffix")}
            </span>
          </div>

          {/* 模型 / 映射摘要（FR-006） */}
          <div className="mt-2 flex flex-wrap items-center gap-1.5">
            {k.mappings.length > 0 ? (
              k.mappings.map((m) => (
                <Badge key={m.id} variant="info" title={t("key.mappingTitle")}>
                  {m.realName} <ArrowRight size={10} /> {m.expectedName}
                </Badge>
              ))
            ) : (
              k.models.slice(0, 4).map((m) => (
                <Badge key={m.realName} variant="neutral">
                  {m.realName}
                </Badge>
              ))
            )}
            {k.mappings.length === 0 && k.models.length > 4 && (
              <span className="text-xs text-text-muted">+{k.models.length - 4}</span>
            )}
          </div>

          <div className="mt-2 text-[11px] text-text-muted">
            {t("key.healthCheckLabel", { time: formatRelativeTime(k.health.lastChecked) })}
            {k.health.latencyMs != null &&
              (k.health.status === "down" ? (
                // 探测失败时这个延迟只是「失败前的往返耗时」，标红并注明，避免被读成探测成功。
                <span className="text-danger">{` · ${k.health.latencyMs}ms · ${t("key.healthProbeFailed")}`}</span>
              ) : (
                ` · ${k.health.latencyMs}ms`
              ))}
          </div>
        </div>

        {/* 右侧操作区 */}
        <div className="flex flex-col items-end gap-2">
          <Switch
            checked={k.enabled}
            onCheckedChange={(v) => void toggleKey(k.id, v)}
            aria-label={t("key.enableAria")}
          />
          {confirmingDelete ? (
            <div className="flex items-center gap-1">
              <Button
                size="sm"
                variant="danger"
                title={t("common.confirmDelete")}
                onClick={() => {
                  setConfirmingDelete(false);
                  void deleteKey(k.id);
                }}
              >
                {t("common.confirmDelete")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                title={t("common.cancel")}
                onClick={() => setConfirmingDelete(false)}
              >
                {t("common.cancel")}
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-0.5">
              <Button
                size="icon"
                variant="ghost"
                title={t("key.checkHealth")}
                onClick={() => void checkHealth(k.id)}
              >
                <RefreshCw size={14} />
              </Button>
              <Button size="icon" variant="ghost" title={t("common.edit")} onClick={() => onEdit(k)}>
                <Pencil size={14} />
              </Button>
              <Button
                size="icon"
                variant="ghost"
                title={t("common.delete")}
                onClick={() => setConfirmingDelete(true)}
              >
                <Trash2 size={14} />
              </Button>
            </div>
          )}
        </div>
      </div>
    </Card>
  );
}
