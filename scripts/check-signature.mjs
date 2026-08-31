#!/usr/bin/env node
/**
 * 产物门：更新包必须**签得对**，不是「签过就行」。
 *
 * # 🔴 为什么要这道门
 *
 * 2026-08-31 出 v0.1.44 时，本机 `npm run tauri build` 的**最后一行**报了
 * `A public key has been found, but no private key`，而在它上面是一长串成功输出
 * （两个 bundle 的路径都打了出来）。产物确实生成了、装得上，于是那次失败被当成
 * 「有个警告但包是好的」—— 而实际上：**没有 `.sig` 的包对 updater 完全不可用**，
 * 用户点「检查更新」永远收不到它。
 *
 * 失效方向是最坏的那种：**构建看起来成功了**。所以判据不能靠人读构建日志的最后一行。
 *
 * # 五条判据
 *
 * 1. updater 已启用且有 pubkey —— 否则下面全部无意义。
 * 2. 每个可更新产物旁边都有 `.sig`。
 * 3. 🔴 **`.sig` 里的 keyid 必须等于客户端内嵌的 pubkey 的 keyid**。这是最要紧的一条：
 *    换钥而没同步 `tauri.conf.json` 的表现是「包签过了、门绿了、所有老用户验签失败」，
 *    而那批用户从此收不到任何更新（本仓 2026-08-16 换过一次钥，台账在 secrets/README.md）。
 * 4. 签名对产物内容**数学上有效**（minisign prehashed：BLAKE2b-512 → Ed25519）。
 *    只查「文件存在」会放过截断的、空的、或配错文件的 `.sig`。
 * 5. `trusted comment` 里的 `file:` 名与产物文件名一致 —— 防「把 A 的 sig 配给 B」。
 *
 * 解析到 0 个产物时**主动判失败**：一个恒绿的门比没有门更糟（同
 * `invoke-command-must-exist` 那条踩过的坑）。
 *
 * # ⚠️ 边界：只覆盖 Windows 的 updater target
 *
 * [`UPDATABLE`] 是 `.msi` / `-setup.exe`。macOS 的 updater target 是 `.app.tar.gz`、
 * Linux 是 `.AppImage`（而 `.dmg` / `.deb` / `.rpm` **不签名**，把它们列进来会误报）。
 * 那两种形态本机无法验证（同 CLAUDE.md 记的「macos-check 是唯一的非 Windows 验证者」），
 * 故 CI 里本门只在 Windows job 跑。
 *
 * 这个边界不影响它真正要守的东西：三个平台共用同一个 `TAURI_SIGNING_PRIVATE_KEY`，
 * 「secret 配没配、配的是不是现役钥」在 Windows 上绿就已经证明了。
 */
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash, createPublicKey, verify } from "node:crypto";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const BUNDLE = join(ROOT, "src-tauri", "target", "release", "bundle");
/** 需要签名的产物后缀（Windows 的 updater target；改平台时同步这里）。 */
const UPDATABLE = [".exe", ".msi"];

const fail = (msg) => {
  console.error(`❌ ${msg}`);
  process.exitCode = 1;
};

/** minisign 公钥/签名的 base64 段 → Buffer。 */
function decodeLine(text, lineIdx) {
  const lines = text.split("\n").filter((l) => l.trim());
  return Buffer.from(lines[lineIdx].trim(), "base64");
}

/** 8 字节小端 keyid → 与 minisign 注释里一致的大端十六进制。 */
const keyIdHex = (buf) => Buffer.from(buf).reverse().toString("hex").toUpperCase();

/** 32 字节裸 Ed25519 公钥 → node 能用的 KeyObject（套标准 SPKI 前缀）。 */
function ed25519PublicKey(raw32) {
  const spkiPrefix = Buffer.from("302a300506032b6570032100", "hex");
  return createPublicKey({
    key: Buffer.concat([spkiPrefix, raw32]),
    format: "der",
    type: "spki",
  });
}

/** 判据 1：updater 已启用且有 pubkey。返回 {keyId, pubKey}。 */
function readConfiguredKey() {
  const conf = JSON.parse(readFileSync(join(ROOT, "src-tauri", "tauri.conf.json"), "utf8"));
  const up = conf?.plugins?.updater;
  if (!up?.active) {
    fail("tauri.conf.json 的 plugins.updater.active 不是 true —— 用户压根收不到更新");
    return null;
  }
  if (!up.pubkey) {
    fail("plugins.updater.pubkey 为空 —— 客户端没有验签依据，任何更新都会被拒");
    return null;
  }
  // pubkey 字段本身是「整份 minisign .pub 文件再 base64 一层」。
  const pubFile = Buffer.from(up.pubkey, "base64").toString("utf8");
  const raw = decodeLine(pubFile, 1); // Ed(2) + keyid(8) + pubkey(32)
  return { keyId: keyIdHex(raw.subarray(2, 10)), pubKey: ed25519PublicKey(raw.subarray(10, 42)) };
}

/** 判据 3~5：单个产物的签名核验。 */
function verifyOne(artifact, sigPath, cfg) {
  const name = basename(artifact);
  const sigFile = Buffer.from(readFileSync(sigPath, "utf8").trim(), "base64").toString("utf8");
  const lines = sigFile.split("\n").filter((l) => l.trim());
  if (lines.length < 4) {
    return fail(`${name}.sig 结构不完整（只有 ${lines.length} 行，minisign 要 4 行）`);
  }
  const sig = Buffer.from(lines[1].trim(), "base64"); // algo(2) + keyid(8) + sig(64)
  const algo = sig.subarray(0, 2).toString("utf8");
  const sigKeyId = keyIdHex(sig.subarray(2, 10));
  const signature = sig.subarray(10, 74);

  // 判据 3：keyid 必须与客户端内嵌的公钥一致。
  if (sigKeyId !== cfg.keyId) {
    return fail(
      `${name} 是用 ${sigKeyId} 签的，而客户端内嵌的公钥是 ${cfg.keyId} —— ` +
        `所有已发布客户端都会验签失败、从此收不到更新。换钥后必须同步 tauri.conf.json 的 pubkey`
    );
  }
  if (signature.length !== 64) {
    return fail(`${name}.sig 的签名段是 ${signature.length} 字节，Ed25519 应为 64`);
  }

  // 判据 4：数学上有效。`ED` = prehashed（先 BLAKE2b-512 再签），`Ed` = 直接签原文。
  const content = readFileSync(artifact);
  const signed =
    algo === "ED" ? createHash("blake2b512").update(content).digest() : content;
  if (algo !== "ED" && algo !== "Ed") {
    return fail(`${name}.sig 的算法标签是 ${algo}，只认 ED / Ed`);
  }
  if (!verify(null, signed, cfg.pubKey, signature)) {
    return fail(`${name} 的签名验不过 —— 产物与签名不匹配（签完又重新构建过？）`);
  }

  // 判据 5：trusted comment 里的文件名必须对得上。
  const trusted = lines[2].replace(/^trusted comment:\s*/, "");
  const inComment = /file:(.+?)(?:\t|$)/.exec(trusted)?.[1]?.trim();
  if (inComment && inComment !== name) {
    return fail(`${name}.sig 的 trusted comment 写的是 file:${inComment} —— 签名配错了产物`);
  }
  // global signature（对 signature||trusted_comment 的签名）也一并验，它是 minisign 用来
  // 防「trusted comment 被改」的那一层；漏验的话时间戳/文件名可被篡改而这道门看不出来。
  const global = Buffer.from(lines[3].trim(), "base64");
  if (!verify(null, Buffer.concat([signature, Buffer.from(trusted, "utf8")]), cfg.pubKey, global)) {
    return fail(`${name}.sig 的 global signature 验不过 —— trusted comment 被改动过`);
  }
  console.log(`✅ ${name} —— 签名有效（keyid ${sigKeyId}，${algo === "ED" ? "prehashed" : "legacy"}）`);
}

const cfg = readConfiguredKey();
// 只查**当次版本**的产物：`bundle/` 不会自动清理，上一版的 exe/msi 一直躺在那里
// （实测 v0.1.43 的两个产物还在），把它们算进来会让门永远红在一件与本次发版无关的事上。
// 同 `audit:release` 的口径（它也只扫当次版本的二进制）。
const VERSION = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8")).version;
if (cfg) {
  if (!existsSync(BUNDLE)) {
    fail(`找不到 ${BUNDLE} —— 先跑 npm run release:build 出包`);
  } else {
    let checked = 0;
    for (const dir of readdirSync(BUNDLE)) {
      const sub = join(BUNDLE, dir);
      for (const f of readdirSync(sub)) {
        if (!UPDATABLE.some((ext) => f.endsWith(ext))) continue;
        if (!f.includes(VERSION)) continue; // 旧版本的残留产物，与本次发版无关
        const artifact = join(sub, f);
        const sigPath = `${artifact}.sig`;
        checked++;
        if (!existsSync(sigPath)) {
          fail(
            `${f} 没有 .sig —— updater 会拒收这个包，用户点「检查更新」永远收不到它。` +
              `原因几乎总是构建时没传 TAURI_SIGNING_PRIVATE_KEY（走 npm run release:build 即可）`
          );
          continue;
        }
        verifyOne(artifact, sigPath, cfg);
      }
    }
    // 恒绿的门比没有门更糟：目录改名 / 后缀变化 / 版本号对不上都会让上面这圈一个都不检查。
    if (checked === 0) {
      fail(
        `在 ${BUNDLE} 下没找到任何 ${VERSION} 版本的 ${UPDATABLE.join(" / ")} 产物 —— ` +
          `本门等于空转。是没出包，还是 package.json 的版本号与产物名不一致？`
      );
    } else if (!process.exitCode) {
      console.log(
        `\n✅ v${VERSION} 的 ${checked} 个可更新产物全部签名有效，keyid 与客户端内嵌公钥一致`
      );
    }
  }
}
