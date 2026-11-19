import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, rename, rm, stat } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { Transform } from "node:stream";
import { pipeline } from "node:stream/promises";

const [model, runtime, output] = process.argv.slice(2);
if (!model || !runtime || !output) throw new Error("Usage: node scripts/laya/prepare-rust-bundle.mjs MODEL_DIRECTORY ORT_DIRECTORY NEW_BUNDLE_DIRECTORY");
const target = resolve(output);
const specification = new URL("./rust-bundle.json", import.meta.url);
const manifest = JSON.parse(await readFile(specification, "utf8"));
if (await stat(target).then(() => true, error => { if (error.code === "ENOENT") return false; throw error; })) throw new Error(`Output already exists: ${target}`);
await mkdir(dirname(target), { recursive: true });
const staged = await mkdtemp(join(dirname(target), `.${basename(target)}-`));
try {
  for (const entry of manifest.files) {
    const [kind, ...relative] = entry.path.split("/");
    const source = join(kind === "model" ? model : runtime, ...relative);
    const dest = join(staged, entry.path);
    if ((await stat(source)).size !== entry.bytes) throw new Error(`Length mismatch: ${entry.path}`);
    await mkdir(dirname(dest), { recursive: true });
    const digest = createHash("sha256");
    const hashStream = new Transform({ transform(chunk, encoding, next) { digest.update(chunk); next(null, chunk); } });
    await pipeline(createReadStream(source), hashStream, createWriteStream(dest, { flags: "wx" }));
    if (digest.digest("hex") !== entry.sha256) throw new Error(`SHA-256 mismatch: ${entry.path}`);
  }
  await copyFile(specification, join(staged, "manifest.json"));
  await rename(staged, target);
  console.log(JSON.stringify({ directory: target, version: manifest.version, verifiedFiles: manifest.files.length, bytes: manifest.files.reduce((total, file) => total + file.bytes, 0) }));
} finally {
  if (dirname(staged) === dirname(target) && basename(staged).startsWith(`.${basename(target)}-`)) await rm(staged, { recursive: true, force: true });
}
