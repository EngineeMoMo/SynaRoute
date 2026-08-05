import { useEffect, useState } from "react";
import { siteConfig } from "@/config/site";
import type { Platform } from "@/data/platforms";

export interface ReleaseAsset {
  name: string;
  url: string;
  size: number;
}

export interface ReleaseInfo {
  version: string;
  publishedAt: string;
  assets: ReleaseAsset[];
  /** api = 实时取到的真实数据；fallback = 请求失败后用的静态兜底 */
  source: "api" | "fallback";
}

export interface ReleaseNote extends ReleaseInfo {
  name: string;
  body: string;
  htmlUrl: string;
}

/** 网络失败时用的兜底：版本号来自配置，资产列表为空 → 下载按钮回落到发布页链接 */
const FALLBACK: ReleaseInfo = {
  version: siteConfig.fallbackVersion,
  publishedAt: "",
  assets: [],
  source: "fallback",
};

// GitHub 匿名 API 限流是 60 次/小时/IP。站内每次路由切换都请求一遍很容易撞上，
// 故在会话内缓存；关掉标签页即失效，不会长期拿着过期版本号。
const CACHE_KEY = "synaroute-latest-release";

function readCache(): ReleaseInfo | null {
  try {
    const raw = sessionStorage.getItem(CACHE_KEY);
    return raw ? (JSON.parse(raw) as ReleaseInfo) : null;
  } catch {
    return null;
  }
}

interface GhAsset {
  name?: unknown;
  browser_download_url?: unknown;
  size?: unknown;
}

function parseAssets(raw: unknown): ReleaseAsset[] {
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((a: GhAsset) => {
    const name = typeof a?.name === "string" ? a.name : "";
    const url = typeof a?.browser_download_url === "string" ? a.browser_download_url : "";
    if (!name || !url) return [];
    return [{ name, url, size: typeof a?.size === "number" ? a.size : 0 }];
  });
}

/**
 * 取最新发布版本。
 *
 * 关键约束：**任何失败路径都必须给出可用结果**。拿不到 API 就用配置里的静态版本号，
 * 下载按钮回落到 releases/latest 页面 —— 官网绝不能因为 GitHub 抽风就变成
 * 一个没有下载入口的页面。
 */
export function useLatestRelease(): { release: ReleaseInfo; loading: boolean } {
  const [release, setRelease] = useState<ReleaseInfo>(() => readCache() ?? FALLBACK);
  const [loading, setLoading] = useState(() => readCache() === null);

  useEffect(() => {
    if (readCache()) return;
    let alive = true;
    const controller = new AbortController();
    // 8 秒还没回来就用兜底，不让下载区一直转圈
    const timer = window.setTimeout(() => controller.abort(), 8000);

    (async () => {
      try {
        const res = await fetch(siteConfig.github.apiLatestRelease, {
          headers: { Accept: "application/vnd.github+json" },
          signal: controller.signal,
        });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = (await res.json()) as Record<string, unknown>;
        const info: ReleaseInfo = {
          version: typeof data.tag_name === "string" ? data.tag_name : FALLBACK.version,
          publishedAt: typeof data.published_at === "string" ? data.published_at : "",
          assets: parseAssets(data.assets),
          source: "api",
        };
        if (!alive) return;
        setRelease(info);
        try {
          sessionStorage.setItem(CACHE_KEY, JSON.stringify(info));
        } catch {
          /* 隐私模式下 sessionStorage 可能不可写，缓存失败不影响功能 */
        }
      } catch {
        if (alive) setRelease(FALLBACK);
      } finally {
        window.clearTimeout(timer);
        if (alive) setLoading(false);
      }
    })();

    return () => {
      alive = false;
      controller.abort();
      window.clearTimeout(timer);
    };
  }, []);

  return { release, loading };
}

export interface ResolvedDownload {
  /** null 表示不可下载 —— 组件必须渲染成不可点的「即将推出」，不能生成假链接 */
  href: string | null;
  size: number;
  /** true 表示没匹配到具体安装包、退而链到发布页 */
  isPageFallback: boolean;
}

/**
 * 把「平台 + 发布信息」解析成一个下载入口。
 *
 * 三种结果：
 * 1. 平台还没做出来（status = coming-soon）→ href 为 null，**绝不给链接**
 * 2. 在发布资产里匹配到安装包 → 直链
 * 3. 匹配不到（API 失败，或该版本确实没传这个平台的包）→ 链到 releases/latest 页面
 */
export function resolveDownload(platform: Platform, release: ReleaseInfo): ResolvedDownload {
  if (platform.status === "coming-soon") {
    return { href: null, size: 0, isPageFallback: false };
  }
  const asset = release.assets.find((a) => platform.assetPattern.test(a.name));
  if (asset) {
    return { href: asset.url, size: asset.size, isPageFallback: false };
  }
  return { href: siteConfig.github.latestRelease, size: 0, isPageFallback: true };
}

/** 更新日志页用：取最近若干个版本的发布说明 */
export function useReleaseNotes(): { notes: ReleaseNote[]; loading: boolean; failed: boolean } {
  const [notes, setNotes] = useState<ReleaseNote[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    const controller = new AbortController();
    const timer = window.setTimeout(() => controller.abort(), 10000);

    (async () => {
      try {
        const res = await fetch(siteConfig.github.apiReleases, {
          headers: { Accept: "application/vnd.github+json" },
          signal: controller.signal,
        });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = (await res.json()) as Record<string, unknown>[];
        if (!Array.isArray(data)) throw new Error("unexpected payload");
        const list: ReleaseNote[] = data.map((r) => ({
          version: typeof r.tag_name === "string" ? r.tag_name : "",
          name: typeof r.name === "string" ? r.name : "",
          body: typeof r.body === "string" ? r.body : "",
          htmlUrl: typeof r.html_url === "string" ? r.html_url : siteConfig.github.releases,
          publishedAt: typeof r.published_at === "string" ? r.published_at : "",
          assets: parseAssets(r.assets),
          source: "api",
        }));
        if (!alive) return;
        setNotes(list.filter((n) => n.version));
      } catch {
        if (alive) setFailed(true);
      } finally {
        window.clearTimeout(timer);
        if (alive) setLoading(false);
      }
    })();

    return () => {
      alive = false;
      controller.abort();
      window.clearTimeout(timer);
    };
  }, []);

  return { notes, loading, failed };
}
