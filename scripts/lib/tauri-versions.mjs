export function checkTauriVersions(packageLock, cargoLock) {
  const problems = [];
  const packages = packageLock?.packages;
  const root = packages?.[""];
  const dependencies = { ...root?.dependencies, ...root?.devDependencies };
  const names = Object.keys(dependencies).filter((name) =>
    name === "@tauri-apps/api" || name === "@tauri-apps/cli" || name.startsWith("@tauri-apps/plugin-")
  );
  const rustVersions = new Map();
  for (const block of String(cargoLock).split(/^\[\[package\]\]\s*$/m).slice(1)) {
    const name = /^name\s*=\s*"([^"]+)"\s*$/m.exec(block)?.[1];
    const version = /^version\s*=\s*"([^"]+)"\s*$/m.exec(block)?.[1];
    if (name && version) rustVersions.set(name, [...(rustVersions.get(name) ?? []), version]);
  }
  if (!names.length) problems.push("No Tauri npm dependencies found in package-lock.json");
  const series = (version) => /^(\d+\.\d+)\.\d+(?:[-+].*)?$/.exec(version ?? "")?.[1];
  for (const name of names) {
    const rustName = name.startsWith("@tauri-apps/plugin-")
      ? name.replace("@tauri-apps/plugin-", "tauri-plugin-") : "tauri";
    const npmVersion = packages?.[`node_modules/${name}`]?.version;
    const versions = rustVersions.get(rustName) ?? [];
    if (!series(npmVersion) || !versions.length || versions.some((version) => !series(version))) {
      problems.push(`Missing or invalid locked version: ${name} (${npmVersion ?? "missing"}) / ${rustName} (${versions.join(", ") || "missing"})`);
    } else if (versions.some((version) => series(version) !== series(npmVersion))) {
      problems.push(`Tauri major/minor mismatch: ${name} ${npmVersion} / ${rustName} ${versions.join(", ")}`);
    }
  }
  return { checked: names.length, problems };
}
