export function normalizeSitePath(pathname: string): string {
  const path = pathname.replace(/\/+$/, "");
  if (path.endsWith("/index.html")) return path.slice(0, -"/index.html".length) || "/";
  if (path.endsWith(".html")) return path.slice(0, -".html".length) || "/";
  return path || "/";
}
