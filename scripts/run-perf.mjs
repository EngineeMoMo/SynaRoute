import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { cpus, homedir, platform, arch } from "node:os";
import { fileURLToPath } from "node:url";
import { parseMeasurements, compareReports } from "./perf-report.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const argumentsList = process.argv.slice(2);
const option = (name) => {
  const index = argumentsList.indexOf(name);
  if (index < 0) return undefined;
  const value = argumentsList[index + 1];
  if (!value || value.startsWith("--")) throw new Error(`Missing value for ${name}`);
  return value;
};
const profile = option("--profile") ?? "release";
if (!["release", "dev"].includes(profile)) throw new Error("--profile must be release or dev");
const output = resolve(root, option("--output") ?? "output/performance/latest.json");
const baselinePath = option("--baseline");
const candidate = join(homedir(), ".cargo", "bin", platform() === "win32" ? "cargo.exe" : "cargo");
const cargo = existsSync(candidate) ? candidate : "cargo";
const toolchain = process.env.SYNAROUTE_PERF_TOOLCHAIN ? [`+${process.env.SYNAROUTE_PERF_TOOLCHAIN}`] : [];
const capture = (command, args) => execFileSync(command, args, { cwd: root, encoding: "utf8", windowsHide: true }).trim();
const rust = capture(cargo, [...toolchain, "--version"]);
const commit = capture("git", ["-c", `safe.directory=${root.replaceAll("\\", "/")}`, "rev-parse", "HEAD"]);
const dirty = capture("git", ["-c", `safe.directory=${root.replaceAll("\\", "/")}`, "status", "--porcelain", "--untracked-files=no"]) !== "";
const baseline = baselinePath ? JSON.parse(readFileSync(resolve(root, baselinePath), "utf8")) : null;
mkdirSync(dirname(output), { recursive: true });
const logPath = output.replace(/\.json$/i, "") + ".log";
writeFileSync(logPath, "");
let combined = "";
for (const filter of ["perf_probe", "proxy::request_body::tests::measure_request_body_reuse"]) {
  const args = [...toolchain, "test", "--locked", "--manifest-path", "src-tauri/Cargo.toml", "--lib",
    ...(profile === "release" ? ["--release"] : []), filter, "--", "--ignored", "--nocapture", "--test-threads=1"];
  await new Promise((accept, reject) => {
    const child = spawn(cargo, args, { cwd: root, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.on("data", (chunk) => { const text = chunk.toString(); combined += text; appendFileSync(logPath, text); process.stdout.write(text); });
    child.stderr.on("data", (chunk) => { appendFileSync(logPath, chunk); process.stderr.write(chunk); });
    child.on("error", reject);
    child.on("exit", (code) => code === 0 ? accept() : reject(new Error(`Performance tests failed (${code})`)));
  });
}
const measurements = parseMeasurements(combined);
for (const expected of ["buffered_single", "large_context_two_keys", "failover_two_keys", "cross_protocol_sse", "upstream_wait", "key_queue", "request_body_reuse", "rss_load", "long_run"]) {
  if (!measurements.some((measurement) => measurement.name === expected)) throw new Error(`Missing measurement: ${expected}`);
}
const report = {
  schemaVersion: 1, recordedAt: new Date().toISOString(), version: JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version,
  commit, dirty, environment: { platform: platform(), arch: arch(), cpu: cpus()[0]?.model ?? "unknown", profile, rust }, measurements,
};
if (baseline) report.comparison = compareReports(baseline, report);
writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
console.log(`Performance report: ${output}`);
if (report.comparison) console.log(report.comparison.compatible ? "Baseline comparison included." : "Different environment: comparisons are informational only.");
