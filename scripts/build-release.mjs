#!/usr/bin/env node
/**
 * 出包入口：**先确认签名密钥可用，再开始构建**，构建完立刻验收。
 *
 * # 🔴 为什么不直接 `npm run tauri build`
 *
 * 2026-08-31 出 v0.1.44 时那条命令跑完 45 秒、打出两个 bundle 路径，**最后一行**才报
 * `A public key has been found, but no private key`。产物确实生成了、装得上，于是那次失败
 * 被当成「有个警告但包是好的」—— 而**没有 `.sig` 的包对 updater 完全不可用**，
 * 用户点「检查更新」永远收不到它。失效方向最坏：构建看起来是成功的。
 *
 * 本脚本把那次失败提前到构建**开始之前**（拿不到私钥就当场退出并说清去哪拿），
 * 并在构建之后自动跑三道产物门 —— 让「签没签成」不再依赖有人去读构建日志的最后一行。
 *
 * # 私钥从哪来（两条，按序）
 *
 * 1. `TAURI_SIGNING_PRIVATE_KEY` 环境变量 —— **CI 走这条**（`release.yml` 从 GitHub
 *    secrets 注入）。tauri 对这个变量既认路径也认密钥内容。
 * 2. 本机 `~/.tauri/synaroute.key` —— `tauri signer generate` 的默认落点。
 *    **tauri 不会自动读它**（那只是生成时的输出位置，不是约定的读取位置），
 *    所以必须由本脚本显式传进去。这正是那次失败的全部原因。
 *
 * # 🔴 绝不打印密钥内容
 *
 * 本脚本只报**来源**（"环境变量" 或文件路径），不 echo 值。理由与 `lan_guard` 那条
 * 「明文令牌进事件等于同时进了三个用户会分享出去的地方」一字不差：构建输出会被贴进
 * issue、截图、聊天记录。同理**不要**手动 `cat ~/.tauri/synaroute.key` —— 那份文件是
 * 单行 base64，`head -1` 会打出整个私钥。
 */
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
/** `tauri signer generate` 在本机的默认落点。用 homedir() 而不是写死路径（有策略门在扫）。 */
const DEFAULT_KEY = join(homedir(), ".tauri", "synaroute.key");

/** 解析签名私钥来源；返回给人看的来源描述，拿不到返回 null。 */
function resolveSigningKey() {
  if (process.env.TAURI_SIGNING_PRIVATE_KEY) {
    return "环境变量 TAURI_SIGNING_PRIVATE_KEY（CI 路径）";
  }
  if (existsSync(DEFAULT_KEY)) {
    process.env.TAURI_SIGNING_PRIVATE_KEY = DEFAULT_KEY;
    return DEFAULT_KEY;
  }
  return null;
}

const from = resolveSigningKey();
if (!from) {
  console.error(
    `❌ 拿不到更新签名私钥，**不开始构建**。\n` +
      `   没有它出的包不带 .sig，装得上但 updater 会拒收 —— 用户点「检查更新」永远收不到。\n\n` +
      `   两条来路（任选其一）：\n` +
      `   1. 本机：把私钥放到 ${DEFAULT_KEY}（这是 tauri signer generate 的默认落点）\n` +
      `   2. 任意位置：export TAURI_SIGNING_PRIVATE_KEY=<私钥路径或内容>\n\n` +
      `   钥匙台账（哪把钥覆盖哪些版本）在 secrets/README.md。\n` +
      `   🔴 现役钥是 7A46ECB8087DE26F，未泄露就不要换 —— 换一次就甩掉一批收不到更新的老用户。`
  );
  process.exit(1);
}

// 本仓那把钥**无口令**（rsign 的 "encrypted secret key" 只是格式标签，口令为空）。
// 不设这个变量时 tauri 会交互式索要口令，在非 tty 下直接挂住。
// 已有值则不覆盖 —— CI 可能真的设了，而覆盖它会让 CI 签名失败。
if (process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD === undefined) {
  process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "";
}

console.log(`🔑 签名私钥来源：${from}`);
console.log("🔨 开始构建（透传参数：" + (process.argv.slice(2).join(" ") || "无") + "）\n");

const build = spawnSync("npm", ["run", "tauri", "build", ...process.argv.slice(2)], {
  cwd: ROOT,
  stdio: "inherit",
  shell: true,
  env: process.env,
});
if (build.status !== 0) {
  console.error(`\n❌ 构建失败（退出码 ${build.status}），不跑产物门。`);
  process.exit(build.status ?? 1);
}

// 构建成功 ≠ 包可发。三道产物门在这里跑，而不是「发完再人工看一眼」：
// - check:embedded  前端资源是否真嵌进 exe（裸 cargo build 的产物该值为 0 → 界面一片空白）
// - check:signature 每个可更新产物都签过、且 keyid 与客户端内嵌公钥一致（真验签）
// - audit:release   运行数据 / 密钥 / 演示数据不得进包
const gates = [
  ["前端嵌入", "check-frontend-embedded.mjs"],
  ["更新签名", "check-signature.mjs"],
  ["外泄审计", "audit-release-bundle.mjs"],
];
let bad = 0;
for (const [label, script] of gates) {
  console.log(`\n──── 产物门：${label} ────`);
  const r = spawnSync(process.execPath, [join(ROOT, "scripts", script)], {
    cwd: ROOT,
    stdio: "inherit",
  });
  if (r.status !== 0) bad++;
}
if (bad > 0) {
  console.error(`\n❌ ${bad} 道产物门未通过 —— 这个包**不要**发出去。`);
  process.exit(1);
}
console.log("\n✅ 出包完成，三道产物门全部通过。");
