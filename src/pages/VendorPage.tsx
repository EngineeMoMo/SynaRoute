import { useEffect, useRef, useState } from "react";
import { useStore } from "@/store";
import { api } from "@/lib/bridge";
import { Card, CardContent } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { BrandIcon, BrandPresetPicker } from "@/components/BrandIcon";
import { useT } from "@/lib/useT";
import { protocolLabel, type Protocol, type Vendor } from "@/types";
import { Building2, Lock, Pencil, Plus, Trash2, ImagePlus, X } from "lucide-react";

const inputCls =
  "h-9 w-full rounded-control border border-border bg-surface px-3 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring";

/** 自定义图标大小上限（data-URL 存进 config.json，过大会撑胖配置）。约 200KB 原图。 */
const MAX_ICON_BYTES = 256 * 1024;

type Draft = { id: string; name: string; defaultBaseUrl: string; defaultProtocol: Protocol; icon?: string };

/** 厂商管理页（FR-002 扩展）：维护可在 Key 编辑器中选择的厂商预设 */
export function VendorPage() {
  const vendors = useStore((s) => s.vendors);
  const loadVendors = useStore((s) => s.loadVendors);
  // 删除失败必须走 toast，不能只写 `error` state —— 那个 state 只在**编辑态**才渲染
  // （见下方 `{editing && ...}` 块里的 `{error && ...}`），而删除是在**列表态**发起的：
  // 后端拒绝删除（如厂商仍被 Key 引用）时，用户点了「删除」后界面一动不动、
  // 没有任何提示，看着像按钮坏了。toast 与页面态无关，一定看得到。
  const showToast = useStore((s) => s.showToast);
  const t = useT();
  // editing.id === "" 表示新增；null 表示未在编辑
  const [editing, setEditing] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    void loadVendors();
  }, [loadVendors]);

  const custom = vendors.filter((v) => !v.builtin);
  const builtin = vendors.filter((v) => v.builtin);

  const startAdd = () => {
    setError(null);
    setEditing({ id: "", name: "", defaultBaseUrl: "", defaultProtocol: "anthropic" });
  };

  const startEdit = (v: Vendor) => {
    setError(null);
    setEditing({ id: v.id, name: v.name, defaultBaseUrl: v.defaultBaseUrl, defaultProtocol: v.defaultProtocol, icon: v.icon });
  };

  // 选图标文件 → 校验大小 → 读为 data-URL 存进草稿（随厂商落 config.json）
  const pickIcon = (file: File | undefined) => {
    if (!editing || !file) return;
    if (file.size > MAX_ICON_BYTES) {
      setError(t("vendor.iconTooLarge", { max: "256KB" }));
      return;
    }
    const reader = new FileReader();
    reader.onload = () => setEditing((cur) => (cur ? { ...cur, icon: String(reader.result) } : cur));
    reader.onerror = () => setError(t("vendor.iconReadFailed"));
    reader.readAsDataURL(file);
  };

  const slugify = (name: string) =>
    name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || `vendor-${Date.now()}`;

  // 新增厂商时按现有 id（含内置）去重，避免同名 slug 相互覆盖
  const uniqueId = (name: string) => {
    const base = slugify(name);
    const taken = new Set(vendors.map((v) => v.id));
    if (!taken.has(base)) return base;
    let n = 2;
    while (taken.has(`${base}-${n}`)) n += 1;
    return `${base}-${n}`;
  };

  const save = async () => {
    if (!editing) return;
    if (!editing.name.trim()) return setError(t("vendor.errNeedName"));
    if (!editing.defaultBaseUrl.trim()) return setError(t("vendor.errNeedUrl"));
    setBusy(true);
    setError(null);
    try {
      const vendor: Vendor = {
        id: editing.id || uniqueId(editing.name),
        name: editing.name.trim(),
        defaultBaseUrl: editing.defaultBaseUrl.trim(),
        defaultProtocol: editing.defaultProtocol,
        builtin: false,
        icon: editing.icon,
      };
      await api.upsertVendor(vendor);
      await loadVendors();
      setEditing(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (v: Vendor) => {
    if (!window.confirm(t("vendor.deleteConfirm", { name: v.name }))) return;
    setBusy(true);
    try {
      await api.deleteVendor(v.id);
      await loadVendors();
    } catch (e) {
      // 双写：toast 保证**列表态**一定看得到（删除就是在列表态发起的，而 `error` state
      // 只在编辑态渲染）；`error` state 保留，供恰好在编辑态时就地显示。
      // 后端拒绝删除（如该厂商仍被 Key 引用）是常见路径，静默会让用户以为按钮坏了。
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      showToast("error", msg);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="flex items-center justify-between border-b border-border px-6 py-4">
        <div>
          <h1 className="text-lg font-semibold text-text-primary">{t("vendor.title")}</h1>
          <p className="mt-0.5 text-sm text-text-secondary">{t("vendor.desc")}</p>
        </div>
        {!editing && (
          <Button onClick={startAdd} disabled={busy}>
            <Plus size={16} /> {t("vendor.add")}
          </Button>
        )}
      </div>

      <div className="space-y-4 p-6">
        {editing && (
          <Card>
            <CardContent className="space-y-3 pt-5">
              {/* 图标：自定义上传（可选）；不传走品牌图标启发式/首字母占位 */}
              <div>
                <label className="mb-1 block text-xs font-medium text-text-secondary">{t("vendor.icon")}</label>
                <div className="flex items-center gap-3">
                  <BrandIcon
                    hint={editing.id || editing.name}
                    fallbackLabel={editing.name || "?"}
                    iconUrl={editing.icon}
                    size={40}
                  />
                  <input
                    ref={fileRef}
                    type="file"
                    accept="image/png,image/jpeg,image/svg+xml,image/webp"
                    className="hidden"
                    onChange={(e) => {
                      pickIcon(e.target.files?.[0]);
                      e.target.value = ""; // 允许重复选同一文件
                    }}
                  />
                  <Button variant="secondary" size="sm" onClick={() => fileRef.current?.click()}>
                    <ImagePlus size={14} /> {t("vendor.iconPick")}
                  </Button>
                  {editing.icon && (
                    <Button variant="ghost" size="sm" onClick={() => setEditing({ ...editing, icon: undefined })}>
                      <X size={14} /> {t("vendor.iconClear")}
                    </Button>
                  )}
                </div>

                {/* 内置品牌预设：**显式挑选**入口（对齐 cc-switch）。
                    为什么必需：自动匹配按 vendor id / 名称猜品牌，覆盖了大多数情况，但中转站
                    的名字千奇百怪（「小明API」「林夕公益站」），猜不中时只能退化成首字母色块
                    —— 用户明知这是个 Claude 中转，却没有任何地方能告诉程序。
                    存的是**预设键字符串**（不是 data-URL），与 cc-switch 的 `icon` 列同一模型，
                    故 `Vendor.icon` 字段无需改动。
                    挑选器本体在 `BrandPresetPicker`，与 Key 编辑器共用一份实现 ——
                    两处各写一遍必然漂移（一边加了新品牌另一边没加）。 */}
                <div className="mt-2">
                  <div className="mb-1.5 text-[11px] text-text-muted">{t("vendor.iconPresetLabel")}</div>
                  <BrandPresetPicker
                    value={editing.icon}
                    onChange={(next) => setEditing({ ...editing, icon: next })}
                  />
                </div>
                <p className="mt-1.5 text-[11px] text-text-muted">{t("vendor.iconHint")}</p>
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-secondary">{t("vendor.name")}</label>
                <input
                  className={inputCls}
                  value={editing.name}
                  onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                  placeholder={t("vendor.namePlaceholder")}
                  autoFocus
                />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-secondary">{t("vendor.baseUrl")}</label>
                <input
                  className={`${inputCls} font-mono`}
                  value={editing.defaultBaseUrl}
                  onChange={(e) => setEditing({ ...editing, defaultBaseUrl: e.target.value })}
                  placeholder="https://api.example.com"
                />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-secondary">{t("vendor.protocol")}</label>
                <select
                  className={inputCls}
                  value={editing.defaultProtocol}
                  onChange={(e) => setEditing({ ...editing, defaultProtocol: e.target.value as Protocol })}
                >
                  <option value="anthropic">Anthropic</option>
                  <option value="openai_chat">OpenAI Chat</option>
                  <option value="openai_responses">OpenAI Responses</option>
                </select>
              </div>
              {error && <p className="text-sm text-danger">{error}</p>}
              <div className="flex justify-end gap-2 pt-1">
                <Button variant="ghost" onClick={() => setEditing(null)} disabled={busy}>
                  {t("vendor.cancel")}
                </Button>
                <Button onClick={save} disabled={busy}>
                  {t("vendor.save")}
                </Button>
              </div>
            </CardContent>
          </Card>
        )}

        {/* 自定义厂商 */}
        {custom.length === 0 && !editing ? (
          <div className="flex flex-col items-center gap-2 py-10 text-text-secondary">
            <Building2 size={28} className="opacity-50" />
            <p className="text-sm">{t("vendor.empty")}</p>
          </div>
        ) : (
          custom.map((v) => (
            <VendorRow key={v.id} vendor={v} onEdit={() => startEdit(v)} onDelete={() => remove(v)} disabled={busy} t={t} />
          ))
        )}

        {/* 内置厂商（只读） */}
        {builtin.map((v) => (
          <VendorRow key={v.id} vendor={v} disabled={busy} t={t} />
        ))}
      </div>
    </div>
  );
}

function VendorRow({
  vendor,
  onEdit,
  onDelete,
  disabled,
  t,
}: {
  vendor: Vendor;
  onEdit?: () => void;
  onDelete?: () => void;
  disabled: boolean;
  t: (k: string, p?: Record<string, string | number>) => string;
}) {
  return (
    <Card>
      <CardContent className="flex items-center gap-3 py-3">
        <BrandIcon hint={vendor.id || vendor.name} fallbackLabel={vendor.name} iconUrl={vendor.icon} size={32} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-medium text-text-primary">{vendor.name}</span>
            {vendor.builtin ? (
              <Badge variant="neutral">
                <Lock size={11} /> {t("vendor.builtin")}
              </Badge>
            ) : (
              <Badge variant="info">{protocolLabel(vendor.defaultProtocol)}</Badge>
            )}
          </div>
          <p className="mt-0.5 truncate font-mono text-xs text-text-secondary">
            {vendor.defaultBaseUrl || "—"}
          </p>
        </div>
        {vendor.builtin ? (
          <span className="text-xs text-text-secondary" title={t("vendor.builtinLocked")}>
            {protocolLabel(vendor.defaultProtocol)}
          </span>
        ) : (
          <div className="flex gap-1">
            <Button variant="ghost" size="icon" onClick={onEdit} disabled={disabled} title={t("vendor.edit")}>
              <Pencil size={16} />
            </Button>
            <Button variant="ghost" size="icon" onClick={onDelete} disabled={disabled} title={t("vendor.delete")}>
              <Trash2 size={16} />
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
