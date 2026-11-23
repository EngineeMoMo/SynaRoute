(async () => {
  const violations = [];
  document.addEventListener("securitypolicyviolation", event => violations.push(event.violatedDirective));
  const inline = document.createElement("script");
  inline.textContent = "window.__cspInlineExecuted = true";
  document.head.append(inline);
  const external = document.createElement("script");
  external.src = "data:text/javascript,window.__cspDataExecuted=true";
  document.head.append(external);
  let evalBlocked = false;
  try { new Function("return 42")(); } catch { evalBlocked = true; }
  try { await fetch("https://example.invalid/csp-probe"); } catch {}
  const image = new Image();
  image.src = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
  let dataImageLoaded = false;
  try { await image.decode(); dataImageLoaded = true; } catch {}
  await new Promise(resolve => setTimeout(resolve, 800));
  await window.__TAURI_INTERNALS__.invoke("csp_report", { result: {
    inlineBlocked: !window.__cspInlineExecuted && violations.includes("script-src-elem"),
    dataScriptBlocked: !window.__cspDataExecuted,
    evalBlocked,
    connectionBlocked: violations.includes("connect-src"),
    assetsLoaded: performance.getEntriesByType("resource").some(entry => entry.name.includes("/assets/") && entry.name.endsWith(".js")),
    rootRendered: !!document.getElementById("root")?.childElementCount,
    dataImageLoaded,
    violations
  }});
})().catch(error => window.__TAURI_INTERNALS__.invoke("csp_report", { result: { error: String(error) }}));
