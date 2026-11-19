import { readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';

const root = resolve(process.argv[2]);
const destination = resolve(process.argv[3] ?? 'output/laya-comparison-2026-11-17.json');
const read = async path => JSON.parse(await readFile(path, 'utf8'));
const median = values => {
  const sorted = [...values].sort((left, right) => left - right);
  return (sorted[(sorted.length - 1) >> 1] + sorted[sorted.length >> 1]) / 2;
};
async function bytes(path) {
  const info = await stat(path);
  if (!info.isDirectory()) return info.size;
  let total = 0;
  for (const name of await readdir(path)) total += await bytes(join(path, name));
  return total;
}
const dataset = await readFile(new URL('./comparison-tasks.json', import.meta.url));
const baseline = await read(join(root, 'python-0.3.29.json'));
const rows = [];
for (const label of ['python-0.3.29', 'python-0.4.35', 'rust-onnx']) {
  const runs = await Promise.all([1, 2, 3].map(round => read(join(root, `repeat-${round}-${label}.json`))));
  const initial = await read(join(root, `${label}.json`));
  const held = runs[0].results.filter(task => task.split === 'held_out');
  rows.push({ label, cached_process_load_ms: runs.map(run => run.load_ms),
    cached_process_load_median_ms: median(runs.map(run => run.load_ms)),
    warm_median_ms: median(runs.map(run => median(run.results.map(task => task.latencies_ms[2])))),
    peak_rss_bytes: initial.peak_rss_bytes ?? initial.sampled_peak_rss_bytes,
    peak_rss_method: label === 'rust-onnx' ? '10ms process sampling; lower bound' : 'Windows peak working set',
    hard_upgraded: held.filter(task => task.hard && task.upgrade).length,
    hard_total: held.filter(task => task.hard).length,
    other_upgraded: held.filter(task => !task.hard && task.upgrade).length,
    other_total: held.filter(task => !task.hard).length,
    max_score_delta: Math.max(...runs.flatMap(run => run.results.map((task, index) => Math.abs(task.score - baseline.results[index].score)))),
    runtime: runs[0].runtime,
  });
}
const packagePlan = await read(join(root, 'install-plan-0.4.35.json'));
const manifestFiles = ['rust-bench/Cargo.lock', 'onnx/multilingual/model.onnx', 'onnx/multilingual/model.onnx.data'];
const hashes = {};
for (const name of manifestFiles) hashes[name] = createHash('sha256').update(await readFile(join(root, name))).digest('hex');
const report = {
  date: '2026-11-17', dataset_sha256: createHash('sha256').update(dataset).digest('hex'), rows,
  scope: 'Fixed production prompt and threshold 0.75; same pinned multilingual checkpoint; two CPU threads; 12 calibration and 36 held-out diagnostic tasks; no retuning. Not downstream answer-quality or paid-provider evaluation.',
  caveats: ['Fresh processes with warmed filesystem cache; first ever cold-disk startup is not controlled.', 'Python 0.4.35 is loaded from pinned source with the same dependency runtime as 0.3.29, not a full clean installation.', 'Rust export shape checks and selected numerical parity passed; histogram binning and Python hooks are not implemented upstream.', 'Runtime sample sizes and short prompts do not establish cross-machine or long-context performance.'],
  footprint: {
    existing_python_environment_bytes: await bytes(resolve(baseline.source, '../../../..')),
    pytorch_checkpoint_bytes: await bytes(baseline.model),
    rust_executable_bytes: await bytes(join(root, 'target/release/synaroute-laya-comparison.exe')),
    onnx_runtime_dll_bytes: await bytes(join(root, 'deps/onnxruntime/capi/onnxruntime.dll')),
    onnx_checkpoint_bytes: await bytes(join(root, 'onnx/multilingual')),
    python_new_install_package_count: packagePlan.install.length,
    python_new_install_versions: Object.fromEntries(packagePlan.install.map(item => [item.metadata.name, item.metadata.version])),
  }, hashes,
  decision: 'Keep the verified 0.3.29 production pin for this change. No classification gain from 0.4.35 on the fixed tasks; its clean resolver changes many packages. Rust/ONNX is a measured candidate for a separate managed-runtime/packaging implementation, not yet a production installer replacement.',
};
await writeFile(destination, JSON.stringify(report, null, 2));
console.log(JSON.stringify({ report: destination, rows, footprint: report.footprint }, null, 2));
