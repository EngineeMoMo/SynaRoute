import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { checkTauriVersions } from "../scripts/lib/tauri-versions.mjs";

function fixture() {
  return {
    packages: {
      "": { dependencies: { "@tauri-apps/api": "~2.12.4", "@tauri-apps/plugin-shell": "2.3.5", "@tauri-apps/plugin-notification": "2.3.3" }, devDependencies: { "@tauri-apps/cli": "~2.12.4" } },
      "node_modules/@tauri-apps/api": { version: "2.12.5" },
      "node_modules/@tauri-apps/cli": { version: "2.12.6" },
      "node_modules/@tauri-apps/plugin-shell": { version: "2.3.5" },
      "node_modules/@tauri-apps/plugin-notification": { version: "2.3.3" },
    },
  };
}

const cargoLock = [
  ["tauri", "2.12.5"],
  ["tauri-plugin-shell", "2.3.5"],
  ["tauri-plugin-notification", "2.3.3"],
].map(([name, version]) => `[[package]]\nname = "${name}"\nversion = "${version}"\n`).join("\n");

describe("Tauri cross-language dependency compatibility", () => {
  it("allows patch differences and reads CLI dev dependencies", () => {
    expect(checkTauriVersions(fixture(), cargoLock.replaceAll("\n", "\r\n"))).toEqual({ checked: 4, problems: [] });
  });

  it.each([
    ["@tauri-apps/plugin-shell", "2.6.0"],
    ["@tauri-apps/plugin-notification", "2.8.1"],
    ["@tauri-apps/api", "3.12.5"],
    ["@tauri-apps/cli", "2.13.0"],
  ])("rejects incompatible %s %s before packaging", (name, version) => {
    const lock = fixture();
    Object.assign(lock.packages, { [`node_modules/${name}`]: { version } });
    expect(checkTauriVersions(lock, cargoLock).problems).toEqual([expect.stringContaining(`${name} ${version}`)]);
  });

  it("fails closed on missing or malformed lockfile entries", () => {
    expect(checkTauriVersions({ packages: {} }, cargoLock).problems).not.toEqual([]);
    expect(checkTauriVersions(fixture(), "").problems).toHaveLength(4);
    const lock = fixture();
    lock.packages["node_modules/@tauri-apps/api"].version = "invalid";
    expect(checkTauriVersions(lock, cargoLock).problems).toEqual([expect.stringContaining("Missing or invalid")]);
    Reflect.deleteProperty(lock.packages, "node_modules/@tauri-apps/api");
    expect(checkTauriVersions(lock, cargoLock).problems).toHaveLength(1);
  });

  it("rejects an additional incompatible Rust version", () => {
    const duplicate = `${cargoLock}\n[[package]]\nname = "tauri-plugin-shell"\nversion = "2.4.0"\n`;
    expect(checkTauriVersions(fixture(), duplicate).problems).toEqual([expect.stringContaining("2.3.5, 2.4.0")]);
  });

  it("validates this repository and is wired into the release policy gate", () => {
    const lock = JSON.parse(readFileSync("package-lock.json", "utf8"));
    const cargo = readFileSync("src-tauri/Cargo.lock", "utf8");
    expect(checkTauriVersions(lock, cargo)).toEqual({ checked: 4, problems: [] });
    expect(readFileSync("scripts/check-forbidden.mjs", "utf8")).toContain("checkTauriVersions(");
  });
});
