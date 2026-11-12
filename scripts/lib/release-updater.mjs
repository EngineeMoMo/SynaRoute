function artifactTargets(version) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error("Invalid release version");
  }
  return [
    [`SynaRoute_${version}_amd64.AppImage`, ["linux-x86_64", "linux-x86_64-appimage"]],
    [`SynaRoute_${version}_amd64.deb`, ["linux-x86_64-deb"]],
    [`SynaRoute-${version}-1.x86_64.rpm`, ["linux-x86_64-rpm"]],
    ["SynaRoute_x64.app.tar.gz", ["darwin-x86_64", "darwin-x86_64-app"]],
    ["SynaRoute_aarch64.app.tar.gz", ["darwin-aarch64", "darwin-aarch64-app"]],
    [`SynaRoute_${version}_x64_en-US.msi`, ["windows-x86_64", "windows-x86_64-msi"]],
    [`SynaRoute_${version}_x64-setup.exe`, ["windows-x86_64-nsis"]],
  ];
}

function requireAsset(assets, name) {
  const matches = assets.filter((asset) => asset.name === name);
  if (matches.length !== 1 || matches[0].state !== "uploaded" || !(matches[0].size > 0)) {
    throw new Error(`Missing, duplicate or incomplete release asset: ${name}`);
  }
  return matches[0];
}

function assetPlan({ version, owner, repo, assets }) {
  return artifactTargets(version).map(([name, platforms]) => {
    const artifact = requireAsset(assets, name);
    const signatureAsset = requireAsset(assets, `${name}.sig`);
    const url = `https://github.com/${owner}/${repo}/releases/download/${encodeURIComponent(`v${version}`)}/${encodeURIComponent(name)}`;
    const draftTag = artifact.browser_download_url?.split("/").at(-2);
    const isDraftUrl = /^untagged-[a-f0-9]+$/.test(draftTag ?? "") &&
      artifact.browser_download_url === `https://github.com/${owner}/${repo}/releases/download/${draftTag}/${encodeURIComponent(name)}`;
    if ((artifact.browser_download_url !== url && !isDraftUrl) || signatureAsset.size > 16384) {
      throw new Error(`Unexpected download URL or signature size: ${name}`);
    }
    return { name, platforms, artifact, signatureAsset, url };
  });
}

function assetText(data) {
  return typeof data === "string" ? data : Buffer.from(data).toString("utf8");
}

function validatedSignature(value, name) {
  const signature = typeof value === "string" ? value.trim() : "";
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(signature)) {
    throw new Error(`Invalid updater signature: ${name}`);
  }
  const lines = Buffer.from(signature, "base64").toString("utf8").split(/\r?\n/).filter(Boolean);
  const packet = Buffer.from(lines[1] ?? "", "base64");
  const algorithm = packet.subarray(0, 2).toString("ascii");
  if (lines.length !== 4 || !lines[0].startsWith("untrusted comment:") ||
      !lines[2].startsWith("trusted comment:") || packet.length !== 74 ||
      !["ED", "Ed"].includes(algorithm) ||
      Buffer.from(lines[3], "base64").length !== 64) {
    throw new Error(`Invalid minisign format: ${name}`);
  }
  return signature;
}

export function buildUpdaterManifest({ version, owner, repo, assets, signatures, pubDate }) {
  if (!Number.isFinite(Date.parse(pubDate))) throw new Error("Invalid updater publication date");
  const platforms = {};
  for (const target of assetPlan({ version, owner, repo, assets })) {
    const signature = validatedSignature(signatures[target.name], target.name);
    for (const platform of target.platforms) platforms[platform] = { signature, url: target.url };
  }
  return { version, notes: "", pub_date: pubDate, platforms };
}

export async function publishUpdaterManifest({ github, owner, repo, releaseId, version }) {
  const scope = { owner, repo };
  const { data: release } = await github.rest.repos.getRelease({ ...scope, release_id: releaseId });
  if (!release.draft || release.tag_name !== `v${version}`) {
    throw new Error("Updater assembly requires the matching draft release");
  }
  const assets = await github.paginate(github.rest.repos.listReleaseAssets, {
    ...scope, release_id: releaseId, per_page: 100,
  });
  const plan = assetPlan({ version, owner, repo, assets });
  const signatures = {};
  const readAsset = (assetId) => github.rest.repos.getReleaseAsset({
    ...scope, asset_id: assetId, headers: { accept: "application/octet-stream" },
  });
  for (const target of plan) {
    const { data } = await readAsset(target.signatureAsset.id);
    const text = assetText(data);
    if (Buffer.byteLength(text, "utf8") !== target.signatureAsset.size) {
      throw new Error(`Incomplete signature download: ${target.name}`);
    }
    signatures[target.name] = text;
  }
  const manifest = buildUpdaterManifest({
    version, owner, repo, assets, signatures, pubDate: new Date().toISOString(),
  });
  const existing = assets.filter((asset) => asset.name === "latest.json");
  if (existing.length > 1) throw new Error("Multiple updater manifests found");
  for (const asset of existing) {
    await github.rest.repos.deleteReleaseAsset({ ...scope, asset_id: asset.id });
  }
  const content = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  const { data: uploaded } = await github.rest.repos.uploadReleaseAsset({
    ...scope, release_id: releaseId, name: "latest.json",
    headers: { "content-type": "application/json" }, data: content,
  });
  if (uploaded.name !== "latest.json" || uploaded.state !== "uploaded" || uploaded.size !== content.length) {
    throw new Error("Updater manifest upload was incomplete");
  }
  const { data: downloaded } = await readAsset(uploaded.id);
  const verified = typeof downloaded === "object" && downloaded !== null &&
    !ArrayBuffer.isView(downloaded) && !(downloaded instanceof ArrayBuffer)
    ? downloaded : JSON.parse(assetText(downloaded));
  if (JSON.stringify(verified) !== JSON.stringify(manifest)) {
    throw new Error("Uploaded updater manifest does not match the complete manifest");
  }
  return { assetId: uploaded.id, platformCount: Object.keys(manifest.platforms).length };
}
