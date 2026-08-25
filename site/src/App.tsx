import { useEffect } from "react";
import {
  BrowserRouter,
  Navigate,
  Outlet,
  Route,
  Routes,
  useLocation,
  useParams,
} from "react-router-dom";
import { Header } from "@/components/layout/Header";
import { Footer } from "@/components/layout/Footer";
import { BackToTop } from "@/components/layout/BackToTop";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { detectLang, isLang } from "@/i18n";
import { scrollToId } from "@/lib/motion";
import { useT } from "@/hooks/useLang";

import HomePage from "@/pages/HomePage";
import DownloadPage from "@/pages/DownloadPage";
import DocsIndexPage from "@/pages/DocsIndexPage";
import DocPage from "@/pages/DocPage";
import ChangelogPage from "@/pages/ChangelogPage";
import LegalPage from "@/pages/LegalPage";
import NotFoundPage from "@/pages/NotFoundPage";

/**
 * 路由切换后的滚动行为。
 *
 * 带 hash 的跳转要滚到对应锚点（从子页面点顶栏的「核心功能」会先跳回首页再滚），
 * 不带 hash 的换页回到顶部。锚点元素可能还没渲染，故用一次 rAF 等布局完成。
 */
function ScrollManager() {
  const { pathname, hash } = useLocation();

  useEffect(() => {
    if (hash) {
      const id = hash.slice(1);
      requestAnimationFrame(() => {
        scrollToId(id);
      });
    } else {
      window.scrollTo({ top: 0 });
    }
  }, [pathname, hash]);

  return null;
}

/** 语言段非法时（如 /fr/download）回落到默认语言，而不是渲染一个 404 */
function LangLayout() {
  const { lang } = useParams();
  const t = useT();
  const location = useLocation();

  if (!isLang(lang)) {
    const rest = location.pathname.split("/").filter(Boolean).slice(1).join("/");
    return <Navigate to={`/${detectLang()}${rest ? `/${rest}` : ""}`} replace />;
  }

  return (
    <>
      {/* 跳过导航直达正文：键盘用户第一个 Tab 就能拿到 */}
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-[60] focus:rounded-control focus:bg-primary-solid focus:px-4 focus:py-2 focus:text-primary-foreground"
      >
        {t("common.skipToContent")}
      </a>
      <Header />
      <main id="main">
        {/*
          ErrorBoundary 包在 `<Outlet />` 外、`<Header>`/`<Footer>` **内**：
          这个位置是刻意选的 —— 内容页出错时顶栏、语言切换、页脚全部保留，
          用户还能自己导航走。包在最外层会让整站一起消失，那正是要避免的
          （实测过：文档页一个 TypeError 让 `#root` 变空，连首页都回不来）。

          `key` 绑路径：出错后切换页面必须重新尝试渲染，否则用户会卡在错误页上
          —— ErrorBoundary 的 state 不会自己因为路由变化而重置。
        */}
        <ErrorBoundary key={location.pathname} label={location.pathname}>
          <Outlet />
        </ErrorBoundary>
      </main>
      <Footer />
      <BackToTop />
    </>
  );
}

export default function App() {
  return (
    <BrowserRouter>
      <ScrollManager />
      <Routes>
        {/* 裸访问 / 时按浏览器语言挑一个 */}
        <Route path="/" element={<Navigate to={`/${detectLang()}`} replace />} />

        <Route path="/:lang" element={<LangLayout />}>
          <Route index element={<HomePage />} />
          <Route path="download" element={<DownloadPage />} />
          <Route path="docs" element={<DocsIndexPage />} />
          <Route path="docs/:slug" element={<DocPage />} />
          <Route path="changelog" element={<ChangelogPage />} />
          <Route path="privacy" element={<LegalPage kind="privacy" />} />
          <Route path="terms" element={<LegalPage kind="terms" />} />
          <Route path="*" element={<NotFoundPage />} />
        </Route>

        {/* 语言段都没有的兜底（如 /foo）：仍要渲染出完整外壳的 404，不能白屏 */}
        <Route path="*" element={<Navigate to={`/${detectLang()}/404`} replace />} />
      </Routes>
    </BrowserRouter>
  );
}
