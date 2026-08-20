// 从工作流输出的 JSON 里取回 confirmed 清单（临时脚本，用完删）。
const fs = require("fs");
const p = "C:/Users/ADMINI~1/AppData/Local/Temp/claude/C--Users-Administrator-Desktop-temp-demo-SynaRoute/6273978c-9bc8-404a-b78b-9a2b0f470290/tasks/wllt6wntj.output";
const top = JSON.parse(fs.readFileSync(p, "utf8"));

// result 可能是字符串（内嵌 JSON）或已是对象
let res = top.result;
if (typeof res === "string") {
  try { res = JSON.parse(res); } catch (e) { console.log("result 不是合法 JSON:", e.message); process.exit(1); }
}
const confirmed = res.confirmed || [];
fs.writeFileSync("fullchain_findings.json", JSON.stringify(res, null, 2));
console.log("恢复 confirmed:", confirmed.length);

// 只列还没修的（按标题关键词粗筛已修项）
const done = /流内 error|熔断阈值|content-encoding|错误体|混合失败池|图片块|截断信号|挂载时快照|上移\/下移|默认 enabled|cc-switch 导入把 priority|用量被记进累加器两次|cached_tokens|密钥库瞬时读失败|Merge 导入覆盖|config\.json 的瞬时读失败|tools::apply 失败|kind "余额"|kind "system"|启动自检事件 kind/;
const rest = confirmed.filter((f) => !done.test(f.title || ""));
console.log("\n=== 剩余未修（按严重度） ===");
for (const sev of ["P0", "P1", "P2", "P3"]) {
  for (const f of rest.filter((x) => x.severity === sev)) {
    console.log(`[${sev}/${f.kind}] ${f.title}`);
    console.log(`    ${f.file}:${f.line}`);
  }
}
