// 单独跑「仓库里有没有私钥材料 / 签名口令」这一条判据。
//
// 与 `npm run check:forbidden` 的关系：那道策略门里包含本检查（每次 gates 与 CI 都跑），
// 这个入口只是方便**改完密钥相关的东西之后立刻自查一次**，不必等整套门。
// 判据实现与全部缘由在 lib/secret-scan.mjs。
import { scanRepo } from "./lib/secret-scan.mjs";

const { findings, stats } = scanRepo();

if (stats.scanned === 0) {
  console.log("❌ 扫到 0 个文件 —— 解析器坏了，本检查形同虚设");
  process.exit(1);
}

console.log(
  `扫过 ${stats.scanned} 个文件（跟踪 ${stats.tracked} 个，跳过二进制 ${stats.skippedBinary}、` +
    `超大 ${stats.skippedLarge}）、${stats.b64Runs} 段 base64、${stats.ciphertext.length} 份密文` +
    (stats.gpgChecks > 0 ? `（试解 ${stats.gpgChecks} 次）` : "（无候选口令，未试解）"),
);

if (!findings.length) {
  console.log("\n✅ 仓库内无私钥材料 / 签名口令");
  console.log("   注意：只扫工作区，不扫 git 历史与已 fork 的副本 —— 绿 ≠ 历史干净。");
  process.exit(0);
}

console.log("");
for (const f of findings) {
  const where = f.file ? `${f.file}${f.line ? `:${f.line}` : ""}` : "(仓库)";
  console.log(`❌ ${where}  ${f.what}`);
}
console.log(`\n❌ ${findings.length} 处。删掉文件能让门变绿，但已公开的密钥必须当作永久失效处置。`);
process.exit(1);
