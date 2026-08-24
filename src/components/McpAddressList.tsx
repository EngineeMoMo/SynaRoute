import { useState } from "react";
import { Button } from "@/components/ui/Button";
import { Copy, Check } from "lucide-react";
import type { CategoryType } from "@/types";

/**
 * 某分类的 MCP 接入地址。**与后端 `mcp::client_url` 必须同形**
 * （`{base}/{分类 wire_id}`）—— 分类段就是服务端识别调用方的唯一途径。
 *
 * 为什么要在前端也有一份：设置页这个复制框是给**手工配置**的用户用的。
 * 它此前给的是裸 `/mcp`（不带分类段），照它配的人会一直被服务端当成「认不出的调用方」
 * 而退回兜底分类 —— 用错 Key 池、日志落错页，且没有任何提示。
 *
 * wire_id 与后端 `CategoryMeta.wire_id` 同值，`CategoryType` 这个联合类型本身就是那份契约
 * （types.ts），故这里直接用它拼，不再维护第二张映射表。
 */
export function mcpCategoryUrl(port: number, category: CategoryType): string {
  return `http://127.0.0.1:${port}/mcp/${category}`;
}

/** 展示顺序：与侧边栏分类顺序一致，用户找起来不用重新建立心智映射。 */
const CATEGORIES: CategoryType[] = ["claude-cli", "codex", "claude-desktop"];

/**
 * 按分类列出三条 MCP 接入地址，各带一个复制按钮。
 *
 * 刻意**不只给一条基址**：三个分类的地址不同，给一条就等于逼用户自己猜该怎么加分类段。
 */
export function McpAddressList({
  port,
  t,
}: {
  port: number;
  t: (key: string, vars?: Record<string, string | number>) => string;
}) {
  // 记住「刚复制了哪一条」而不是一个 bool：三行共用一个 bool 会让点第一行时
  // 三行的按钮一起变成「已复制」。
  const [copied, setCopied] = useState<CategoryType | null>(null);
  const handleCopy = (c: CategoryType, url: string) => {
    void navigator.clipboard?.writeText(url).then(() => {
      setCopied(c);
      setTimeout(() => setCopied((cur) => (cur === c ? null : cur)), 1400);
    });
  };

  return (
    <div className="space-y-1.5">
      {CATEGORIES.map((c) => {
        const url = mcpCategoryUrl(port, c);
        return (
          <div key={c} className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-xs font-medium text-text-secondary">{t(`nav.${c}`)}</div>
              <div className="break-all font-mono text-xs text-text-muted">{url}</div>
            </div>
            <Button
              size="sm"
              variant="secondary"
              className="shrink-0"
              onClick={() => handleCopy(c, url)}
            >
              {copied === c ? <Check size={14} /> : <Copy size={14} />}
              {copied === c ? t("settings.mcpCopied") : t("settings.mcpCopy")}
            </Button>
          </div>
        );
      })}
    </div>
  );
}
