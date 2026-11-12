import { describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { buildUpdaterManifest, publishUpdaterManifest } from "../scripts/lib/release-updater.mjs";

const version = "0.1.84";
const owner = "Example";
const repo = "SynaRoute";
const pubDate = "2026-11-11T08:45:00.000Z";
const artifactNames = [
  `SynaRoute_${version}_amd64.AppImage`, `SynaRoute_${version}_amd64.deb`,
  `SynaRoute-${version}-1.x86_64.rpm`, "SynaRoute_x64.app.tar.gz", "SynaRoute_aarch64.app.tar.gz",
  `SynaRoute_${version}_x64_en-US.msi`, `SynaRoute_${version}_x64-setup.exe`,
];

function fixtures({ draft = false } = {}) {
  const downloadTag = draft ? "untagged-96cb622799f0fb03553e" : `v${version}`;
  const packet = Buffer.alloc(74, 1);
  packet.write("ED", 0, "ascii");
  const signature = Buffer.from([
    "untrusted comment: test signature", packet.toString("base64"),
    "trusted comment: test fixture", Buffer.alloc(64, 2).toString("base64"), "",
  ].join("\n")).toString("base64");
  const assets = artifactNames.flatMap((name, index) => [
    { id: index * 2 + 1, name, state: "uploaded", size: 100, browser_download_url: `https://github.com/${owner}/${repo}/releases/download/${downloadTag}/${name}` },
    { id: index * 2 + 2, name: `${name}.sig`, state: "uploaded", size: signature.length, browser_download_url: `https://github.com/${owner}/${repo}/releases/download/${downloadTag}/${name}.sig` },
  ]);
  return { version, owner, repo, pubDate, assets, signatures: Object.fromEntries(artifactNames.map((name) => [name, signature])) };
}

function githubFixture({ roundTripObject = false } = {}) {
  const inputs = fixtures({ draft: true });
  const stored = new Map<number, Buffer | string>();
  for (const asset of inputs.assets.filter((asset) => asset.name.endsWith(".sig"))) {
    stored.set(asset.id, inputs.signatures[asset.name.slice(0, -4)]);
  }
  const api = {
    getRelease: vi.fn(async () => ({ data: { draft: true, tag_name: `v${version}` } })),
    listReleaseAssets: vi.fn(),
    getReleaseAsset: vi.fn(async ({ asset_id }: { asset_id: number }) => {
      const value = stored.get(asset_id);
      return { data: roundTripObject && asset_id === 1000 ? JSON.parse(String(value)) : value };
    }),
    deleteReleaseAsset: vi.fn(async () => ({})),
    uploadReleaseAsset: vi.fn(async ({ data }: { data: Buffer }) => {
      stored.set(1000, data);
      return { data: { id: 1000, name: "latest.json", state: "uploaded", size: data.length } };
    }),
  };
  const github = { rest: { repos: api }, paginate: vi.fn(async () => inputs.assets) };
  return { inputs, api, github, options: { github, owner, repo, releaseId: 1, version } };
}

describe("complete release updater manifest", () => {
  it("includes all eleven aliases and pins every URL to the release tag", () => {
    const inputs = fixtures();
    const manifest = buildUpdaterManifest(inputs);
    expect(Object.keys(manifest.platforms)).toHaveLength(11);
    expect(manifest.platforms["windows-x86_64"].url.endsWith("_x64_en-US.msi")).toBe(true);
    expect(manifest.platforms["linux-x86_64"].url.endsWith("_amd64.AppImage")).toBe(true);
    expect(manifest.platforms["darwin-x86_64"]).toEqual(manifest.platforms["darwin-x86_64-app"]);
    expect(manifest.platforms["darwin-aarch64"]).toEqual(manifest.platforms["darwin-aarch64-app"]);
    for (const entry of Object.values(manifest.platforms) as Array<{ url: string; signature: string }>) {
      expect(entry.url).toContain(`/releases/download/v${version}/`);
      expect(entry.signature).toBe(inputs.signatures[artifactNames[0]]);
    }
  });

  it("converts GitHub draft asset URLs into fixed release-tag URLs", () => {
    const inputs = fixtures({ draft: true });
    const manifest = buildUpdaterManifest(inputs);
    expect(Object.keys(manifest.platforms)).toHaveLength(11);
    for (const entry of Object.values(manifest.platforms) as Array<{ url: string }>) {
      expect(entry.url).toContain(`/releases/download/v${version}/`);
      expect(entry.url).not.toContain("untagged-");
    }
  });

  it.each([
    `https://example.test/${owner}/${repo}/releases/download/untagged-96cb622799f0fb03553e/${artifactNames[0]}`,
    `https://github.com/${owner}/Other/releases/download/untagged-96cb622799f0fb03553e/${artifactNames[0]}`,
    `https://github.com/${owner}/${repo}/releases/download/v0.1.83/${artifactNames[0]}`,
    `https://github.com/${owner}/${repo}/releases/download/untagged-96cb622799f0fb03553e/other.AppImage`,
    `https://github.com/${owner}/${repo}/releases/download/untagged-96cb622799f0fb03553e/${artifactNames[0]}?download=1`,
  ])("rejects an unexpected draft download location: %s", (url) => {
    const inputs = fixtures({ draft: true });
    inputs.assets[0].browser_download_url = url;
    expect(() => buildUpdaterManifest(inputs)).toThrow("Unexpected download URL");
  });

  it.each(artifactNames)("rejects a missing artifact: %s", (name) => {
    const inputs = fixtures();
    inputs.assets = inputs.assets.filter((asset) => asset.name !== name);
    expect(() => buildUpdaterManifest(inputs)).toThrow("Missing, duplicate or incomplete");
  });

  it("rejects missing signature files, duplicates and incomplete uploads", () => {
    const missing = fixtures();
    missing.assets = missing.assets.filter((asset) => asset.name !== `${artifactNames[0]}.sig`);
    expect(() => buildUpdaterManifest(missing)).toThrow();
    const duplicate = fixtures();
    duplicate.assets.push({ ...duplicate.assets[0] });
    expect(() => buildUpdaterManifest(duplicate)).toThrow();
    const incomplete = fixtures();
    incomplete.assets[0].state = "starter";
    expect(() => buildUpdaterManifest(incomplete)).toThrow();
  });

  it("rejects malformed signatures, invalid dates and rolling download URLs", () => {
    const invalid = fixtures();
    invalid.signatures[artifactNames[0]] = "not a signature";
    expect(() => buildUpdaterManifest(invalid)).toThrow("Invalid updater signature");
    const malformed = fixtures();
    malformed.signatures[artifactNames[0]] = Buffer.from("not minisign").toString("base64");
    expect(() => buildUpdaterManifest(malformed)).toThrow("Invalid minisign format");
    const rolling = fixtures();
    rolling.assets[0].browser_download_url = rolling.assets[0].browser_download_url.replace(`/download/v${version}/`, "/latest/download/");
    expect(() => buildUpdaterManifest(rolling)).toThrow("Unexpected download URL");
    expect(() => buildUpdaterManifest({ ...fixtures(), pubDate: "invalid" })).toThrow("Invalid updater publication date");
  });

  it.each([false, true])("uploads once and verifies complete round-trip data (object=%s)", async (roundTripObject) => {
    const { api, github, options } = githubFixture({ roundTripObject });
    const result = await publishUpdaterManifest(options);
    expect(result).toEqual({ assetId: 1000, platformCount: 11 });
    expect(github.paginate).toHaveBeenCalledWith(api.listReleaseAssets, { owner, repo, release_id: 1, per_page: 100 });
    expect(api.getReleaseAsset).toHaveBeenCalledTimes(8);
    expect(api.uploadReleaseAsset).toHaveBeenCalledTimes(1);
    expect(api.deleteReleaseAsset).not.toHaveBeenCalled();
  });

  it("does not mutate a release when an artifact or downloaded signature is missing", async () => {
    const missing = githubFixture();
    missing.inputs.assets.pop();
    await expect(publishUpdaterManifest(missing.options)).rejects.toThrow();
    expect(missing.api.uploadReleaseAsset).not.toHaveBeenCalled();
    expect(missing.api.deleteReleaseAsset).not.toHaveBeenCalled();
    const truncated = githubFixture();
    truncated.api.getReleaseAsset.mockResolvedValueOnce({ data: "truncated" });
    await expect(publishUpdaterManifest(truncated.options)).rejects.toThrow("Incomplete signature download");
    expect(truncated.api.uploadReleaseAsset).not.toHaveBeenCalled();
  });

  it("refuses published or mismatched releases", async () => {
    for (const release of [{ draft: false, tag_name: `v${version}` }, { draft: true, tag_name: "v0.1.83" }]) {
      const { api, options } = githubFixture();
      api.getRelease.mockResolvedValueOnce({ data: release });
      await expect(publishUpdaterManifest(options)).rejects.toThrow("matching draft release");
      expect(api.uploadReleaseAsset).not.toHaveBeenCalled();
      expect(api.deleteReleaseAsset).not.toHaveBeenCalled();
    }
  });

  it("validates every signature before replacing an earlier draft manifest", async () => {
    const { inputs, api, options } = githubFixture();
    inputs.assets.push({ id: 999, name: "latest.json", state: "uploaded", size: 100, browser_download_url: "https://example.test/latest.json" });
    await publishUpdaterManifest(options);
    expect(api.deleteReleaseAsset).toHaveBeenCalledWith({ owner, repo, asset_id: 999 });
    expect(api.deleteReleaseAsset.mock.invocationCallOrder[0]).toBeGreaterThan(api.getReleaseAsset.mock.invocationCallOrder[6]);
    expect(api.uploadReleaseAsset.mock.invocationCallOrder[0]).toBeGreaterThan(api.deleteReleaseAsset.mock.invocationCallOrder[0]);
    const broken = githubFixture();
    broken.inputs.assets.push({ ...inputs.assets[inputs.assets.length - 1] });
    broken.api.getReleaseAsset.mockResolvedValueOnce({ data: "truncated" });
    await expect(publishUpdaterManifest(broken.options)).rejects.toThrow();
    expect(broken.api.deleteReleaseAsset).not.toHaveBeenCalled();
    expect(broken.api.uploadReleaseAsset).not.toHaveBeenCalled();
  });

  it("rejects an incomplete manifest upload before attempting readback", async () => {
    const { api, options } = githubFixture();
    api.uploadReleaseAsset.mockResolvedValueOnce({ data: { id: 1000, name: "latest.json", state: "starter", size: 1 } });
    await expect(publishUpdaterManifest(options)).rejects.toThrow("upload was incomplete");
    expect(api.getReleaseAsset).toHaveBeenCalledTimes(7);
  });

  it("fails verification when the uploaded manifest is truncated", async () => {
    const { api, options } = githubFixture();
    const readAsset = api.getReleaseAsset.getMockImplementation()!;
    api.getReleaseAsset.mockImplementation(async (request) => request.asset_id === 1000
      ? { data: JSON.stringify({ version, platforms: {} }) } : readAsset(request));
    await expect(publishUpdaterManifest(options)).rejects.toThrow("does not match");
  });

  it("assembles the manifest only after every platform build, before publishing", () => {
    const workflow = readFileSync(".github/workflows/release.yml", "utf8");
    const publishJob = workflow.slice(workflow.indexOf("  publish-release:"));
    expect(workflow).toContain("includeUpdaterJson: false");
    expect(workflow).not.toContain("includeUpdaterJson: true");
    expect(publishJob).toContain("needs: [create-release, build-tauri]");
    expect(publishJob).toContain("await publishUpdaterManifest(");
    expect(publishJob.indexOf("await publishUpdaterManifest(")).toBeLessThan(publishJob.indexOf("draft: false"));
    expect(publishJob).toContain("await github.rest.repos.updateRelease(");
  });

  it("executes the actual workflow assembly script against the GitHub contract", async () => {
    const workflow = readFileSync(".github/workflows/release.yml", "utf8").replaceAll("\r\n", "\n");
    const step = workflow.split("      - name: assemble complete updater manifest\n")[1].split("      - name: publish release\n")[0];
    const script = step.split("          script: |\n")[1].trimEnd().split("\n").map((line) => line.slice(12)).join("\n");
    const child = `
      import assert from "node:assert/strict";
      import { createRequire } from "node:module";
      const inputs = ${JSON.stringify(fixtures({ draft: true }))};
      const stored = new Map();
      for (const asset of inputs.assets.filter(asset => asset.name.endsWith(".sig"))) {
        stored.set(asset.id, inputs.signatures[asset.name.slice(0, -4)]);
      }
      let uploadCount = 0;
      const github = { paginate: async () => inputs.assets, rest: { repos: {
        getRelease: async () => ({ data: { draft: true, tag_name: "v" + inputs.version } }),
        listReleaseAssets: () => {},
        getReleaseAsset: async ({ asset_id }) => ({ data: stored.get(asset_id) }),
        deleteReleaseAsset: async () => assert.fail("Unexpected manifest deletion"),
        uploadReleaseAsset: async ({ data }) => {
          uploadCount++;
          stored.set(1000, JSON.parse(data.toString("utf8")));
          return { data: { id: 1000, name: "latest.json", state: "uploaded", size: data.length } };
        },
      } } };
      const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
      const execute = new AsyncFunction("require", "github", "context", "process", "core", ${JSON.stringify(script)});
      const nativeRequire = createRequire(import.meta.url);
      const requireModule = name => name.endsWith("/package.json") ? { version: inputs.version } : nativeRequire(name);
      const logs = [];
      await execute(requireModule, github, { repo: { owner: inputs.owner, repo: inputs.repo } }, {
        env: { GITHUB_WORKSPACE: ${JSON.stringify(resolve(".").replaceAll("\\", "/"))}, release_id: "1" },
      }, { info: message => logs.push(message) });
      assert.equal(uploadCount, 1);
      assert.deepEqual(logs, ["Verified updater manifest with 11 platform entries."]);
    `;
    expect(execFileSync(process.execPath, ["--input-type=module", "-e", child], {
      encoding: "utf8", timeout: 20000,
    })).toBe("");
  });
});
