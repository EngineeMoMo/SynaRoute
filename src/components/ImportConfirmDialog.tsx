import { useEffect, useMemo, useState } from "react";
import { DownloadCloud } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Combobox } from "@/components/ui/Combobox";
import { DialogBody, DialogFooter, DialogFrame, DialogHeader } from "@/components/ui/DialogFrame";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import type { CategoryType, Protocol } from "@/types";

const CATEGORIES: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];
const PROTOCOLS: Protocol[] = ["anthropic", "openai_chat", "openai_responses"];

/**
 * synaroute:// 深链接一键导入的确认框。
 *
 * 🔴 绝不静默导入：URL 谁都能构造，必须由用户当场确认才落盘（同 lan_guard 纪律）。
 * 展示端点**域名**让用户看清密钥会被发去哪（防钓鱼站诱导导入把密钥转走的端点）。
 * category/protocol 若 URL 没带（预览里为 null），用下拉让用户选、选齐才可导入。
 */
export function ImportConfirmDialog() {
  const preview = useStore((s) => s.deeplinkImport);
  const setPreview = useStore((s) => s.setDeeplinkImport);
  const refreshCategory = useStore((s) => s.refreshCategory);
  const showToast = useStore((s) => s.showToast);
  const t = useT();

  // 分类/协议一律给下拉，让用户能改；链接带了就**预选**它、没带就留空。
  const [category, setCategory] = useState<CategoryType | "">("");
  const [protocol, setProtocol] = useState<Protocol | "">("");
  const [busy, setBusy] = useState(false);

  // 🔴 必须用 effect 从 preview 预填，不能写 useState(preview?.category ?? "")：本组件在 App 里
  // 常驻挂载，挂载那一刻 preview 还是 null → 初值恒为 ""，而 useState 初值不会随 preview 变为对象
  // 重跑。于是「链接带了 category」时状态仍停在 ""，canImport 判空 → 「导入」按钮永远禁用。
  // 依赖 [preview]：一次导入内 preview 对象不变，用户改过的选择不会被这段清掉；新链接到达才重填。
  useEffect(() => {
    setCategory(preview?.category ?? "");
    setProtocol(preview?.protocol ?? "");
  }, [preview]);

  // 端点域名：只显示 host，让用户一眼看清密钥去向（不显示完整 URL 免得混入路径里的 token）。
  const host = useMemo(() => {
    try {
      return new URL(preview?.baseUrl ?? "").host;
    } catch {
      return preview?.baseUrl ?? "";
    }
  }, [preview?.baseUrl]);

  if (!preview) return null;

  const canImport = category !== "" && protocol !== "" && !busy;

  const close = () => {
    void api.deeplinkImportDiscard();
    setPreview(null);
  };

  const doImport = async () => {
    if (category === "" || protocol === "") return;
    setBusy(true);
    try {
      await api.deeplinkImportApply(category, protocol);
      showToast("success", t("import.done", { name: preview.name || t("import.unnamed"), cat: t(`nav.${category}`) }));
      setPreview(null);
      await refreshCategory();
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <DialogFrame
      size="sm"
      onBackdropMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) close();
      }}
    >
      <DialogHeader>
        <div className="flex items-center gap-2">
          <span className="flex h-8 w-8 items-center justify-center rounded-control bg-primary/12 text-primary">
            <DownloadCloud size={16} aria-hidden="true" />
          </span>
          <h2 className="text-sm font-semibold text-text-primary">{t("import.title")}</h2>
        </div>
      </DialogHeader>
      <DialogBody className="flex flex-col gap-3">
        <p className="text-sm leading-relaxed text-text-secondary">{t("import.desc")}</p>

        <dl className="flex flex-col gap-2 text-sm">
          <Row label={t("import.name")}>
            <span className="text-text-primary">{preview.name || t("import.unnamed")}</span>
          </Row>

          <Row label={t("import.endpoint")}>
            <span className="font-mono text-text-primary">{host}</span>
          </Row>

          <Row label={t("import.category")}>
            <Combobox
              value={category}
              options={CATEGORIES}
              onChange={(v) => setCategory(v as CategoryType)}
              placeholder={t("import.pickCategory")}
              allowCustom={false}
              emptyHint={t("import.pickCategory")}
            />
          </Row>

          <Row label={t("import.protocol")}>
            <Combobox
              value={protocol}
              options={PROTOCOLS}
              onChange={(v) => setProtocol(v as Protocol)}
              placeholder={t("import.pickProtocol")}
              allowCustom={false}
              emptyHint={t("import.pickProtocol")}
            />
          </Row>

          <Row label={t("import.secret")}>
            <span className={preview.hasSecret ? "text-success" : "text-warning"}>
              {preview.hasSecret ? t("import.secretYes") : t("import.secretNo")}
            </span>
          </Row>
        </dl>

        {preview.extraEndpoints.length > 0 && (
          <p className="text-xs leading-relaxed text-text-muted">
            {t("import.extraEndpoints", { n: preview.extraEndpoints.length })}
          </p>
        )}
        <p className="text-xs leading-relaxed text-text-muted">{t("import.enabledNote")}</p>
      </DialogBody>
      <DialogFooter>
        <Button variant="ghost" onClick={close} disabled={busy} autoFocus>
          {t("common.cancel")}
        </Button>
        <Button onClick={doImport} disabled={!canImport}>
          {busy ? t("import.importing") : t("import.action")}
        </Button>
      </DialogFooter>
    </DialogFrame>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <dt className="shrink-0 text-text-muted">{label}</dt>
      <dd className="min-w-0 text-right">{children}</dd>
    </div>
  );
}
