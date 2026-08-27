// 「自定义请求头」的前端即时校验（B1，docs/14 §21.1）。
//
// 与后端 `custom_headers::{is_reserved, reject_reserved}` 是**同一份体检的两侧**：
// 界面上即时提示的、与保存时真正拦下的，必须是同一批头名字、同一个原因。
// 两边各自 filter 是这类功能最典型的漂移源 —— 用户按界面提示改好了，保存仍被拒
// （本仓在桌面端模型名那条上已经踩过一次，故那里也是共用一份 `desktop_model_name_report`）。
//
// 🔴 这份清单与 Rust 的 `is_reserved` 由 `tests/reservedHeadersParity.test.ts`
// 机械对账 —— 编译器管不到跨语言这条缝，而分叉是静默的：
// 前端少一条 = 用户能填进去、保存被拒但界面没提示；前端多一条 = 界面拦了本可用的头。

/** 代理自有、不许用户覆盖的头。**顺序无关**，判据只比集合。 */
export const RESERVED_HEADERS = [
  "authorization",
  "x-api-key",
  "anthropic-version",
  "host",
  "content-length",
  "content-type",
  "accept-encoding",
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
] as const;

const RESERVED = new Set<string>(RESERVED_HEADERS);

export type HeadersCheck =
  | { kind: "empty" }
  | { kind: "ok"; count: number }
  | { kind: "invalid"; reason: string }
  | { kind: "reserved"; names: string[] };

/**
 * 体检一段 `headers_json`。**不抛异常** —— 用户边打字边校验，中间态必然大量非法。
 *
 * 判据与后端 `custom_headers::parse` 对齐：必须是 JSON 对象、值为字符串/数字/布尔、
 * 头名字只允许 `[a-z0-9-_.]`、值里不许有 CR/LF（请求头注入）。
 */
export function checkCustomHeaders(raw: string): HeadersCheck {
  const text = raw.trim();
  if (!text) return { kind: "empty" };

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    return { kind: "invalid", reason: e instanceof Error ? e.message : String(e) };
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { kind: "invalid", reason: '必须是 JSON 对象，形如 {"X-Title": "MyApp"}' };
  }

  const entries = Object.entries(parsed as Record<string, unknown>);
  const reserved: string[] = [];
  for (const [k, v] of entries) {
    const name = k.trim().toLowerCase();
    if (!name) return { kind: "invalid", reason: "有空的头名字" };
    if (!/^[a-z0-9\-_.]+$/.test(name)) {
      return { kind: "invalid", reason: `头名字 \`${k}\` 含非法字符（只允许字母、数字、- _ .）` };
    }
    if (typeof v === "object" || typeof v === "undefined") {
      return { kind: "invalid", reason: `头 \`${k}\` 的值必须是字符串（或数字/布尔）` };
    }
    if (typeof v === "string" && /[\r\n\0]/.test(v)) {
      return { kind: "invalid", reason: `头 \`${k}\` 的值含换行，会造成请求头注入` };
    }
    if (RESERVED.has(name)) reserved.push(name);
  }
  if (reserved.length) return { kind: "reserved", names: reserved };
  return { kind: "ok", count: entries.length };
}
