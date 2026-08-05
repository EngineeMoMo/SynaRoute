import { useCallback } from "react";
import { useLocation, useParams } from "react-router-dom";
import { DEFAULT_LANG, isLang, translate, type Lang, type TFunc } from "@/i18n";

/**
 * 当前语言来自 URL 的第一段（/zh/... 或 /en/...）。
 *
 * 放在 URL 里而不是 localStorage，有两个实际好处：分享出去的链接自带语言，
 * 且搜索引擎能把中英两版当作两个可收录页面（配合 hreflang）。
 */
export function useLang(): Lang {
  const { lang } = useParams();
  return isLang(lang) ? lang : DEFAULT_LANG;
}

/** 组件内取词 */
export function useT(): TFunc {
  const lang = useLang();
  return useCallback<TFunc>((key, vars) => translate(lang, key, vars), [lang]);
}

/**
 * 拼一个带语言前缀的站内路径。
 * `to("download")` → `/zh/download`；`to("")` → `/zh`
 */
export function useLocalizedPath() {
  const lang = useLang();
  return useCallback(
    (to: string) => {
      const clean = to.replace(/^\/+/, "");
      return clean ? `/${lang}/${clean}` : `/${lang}`;
    },
    [lang]
  );
}

/**
 * 切语言时保持当前页面位置：把路径里的语言段换掉，其余原样保留。
 * 例如在 /zh/docs/cli 切到英文 → /en/docs/cli，而不是回首页。
 */
export function useSwitchLangPath() {
  const { pathname, search, hash } = useLocation();
  return useCallback(
    (next: Lang) => {
      const segments = pathname.split("/").filter(Boolean);
      if (segments.length && isLang(segments[0])) {
        segments[0] = next;
      } else {
        segments.unshift(next);
      }
      return `/${segments.join("/")}${search}${hash}`;
    },
    [pathname, search, hash]
  );
}
