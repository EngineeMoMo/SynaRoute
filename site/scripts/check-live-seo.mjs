import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";
import { checkPublicSite, origin } from "./check-seo.mjs";

const execute = promisify(execFile);
const curl = process.platform === "win32" ? "curl.exe" : "curl";
const marker = "\n__SYNAROUTE_RESPONSE__\n";

async function request(path, expectedStatus = 200) {
  const url = new URL(path, origin).href;
  assert.equal(new URL(url).origin, origin);
  for (let attempt = 1; ; attempt++) {
    try {
      const { stdout } = await execute(curl, [
        "--silent", "--show-error", "--max-time", "25", "--connect-timeout", "10",
        "--header", "Cache-Control: no-cache",
        "--user-agent", "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
        "--write-out", `${marker}%{response_code}\n%{header_json}`, url,
      ], { encoding: "utf8", maxBuffer: 4 * 1024 * 1024, windowsHide: true });
      const start = stdout.lastIndexOf(marker);
      assert.ok(start >= 0, `${url} 缺少 HTTP 响应信息`);
      const [code, ...headerLines] = stdout.slice(start + marker.length).split("\n");
      const status = Number(code);
      const headers = JSON.parse(headerLines.join("\n"));
      if ((status === 429 || status >= 500) && attempt < 3) {
        await delay(1000 * attempt);
        continue;
      }
      const location = headers.location?.join(", ");
      assert.equal(status, expectedStatus, `HTTP ${status}${location ? ` → ${location}` : ""} (${url})`);
      if (expectedStatus === 200) {
        assert.doesNotMatch((headers["x-robots-tag"] ?? []).join(","), /\b(noindex|none)\b/i, `${url} 响应头禁止索引`);
      }
      return stdout.slice(0, start);
    } catch (error) {
      if (attempt >= 3 || ![6, 7, 18, 28, 35, 52, 55, 56, 92].includes(error.code)) throw error;
      await delay(1000 * attempt);
    }
  }
}

try {
  await request("/not-a-synaroute-page", 404);
  const count = await checkPublicSite(request);
  console.log(`[check-live-seo] 已验证正式官网 404、robots、sitemap、根首页与 ${count} 个规范网页`);
} catch (error) {
  console.error(`[check-live-seo] ${error.message}`);
  process.exitCode = 1;
}
