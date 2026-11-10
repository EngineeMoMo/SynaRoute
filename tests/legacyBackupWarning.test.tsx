import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { LegacyBackupWarning } from "../src/components/LegacyBackupWarning";
import { BackupImportPreview } from "../src/components/BackupImportPreview";
import { translate, type Lang } from "../src/lib/i18n";

function renderWarning(count: number, busy = false, lang: Lang = "en") {
  return renderToStaticMarkup(createElement(LegacyBackupWarning, {
    count,
    busy,
    onSanitize: () => {},
    t: (key, variables) => translate(lang, key, variables),
  }));
}

describe("legacy backup credential warning", () => {
  it("does not flag reference-only backups", () => {
    expect(renderWarning(0)).toBe("");
  });

  it("warns before import and offers a non-destructive copy", () => {
    const markup = renderWarning(2);
    expect(markup).toContain('role="alert"');
    expect(markup).toContain("Detected 2 plaintext balance-panel token(s)");
    expect(markup).toContain("Revoke or rotate");
    expect(markup).toContain("No import or vault unlock is needed");
    expect(markup).toContain("never overwritten");
    expect(markup).toContain("Save a copy without plaintext tokens");
    expect(markup).not.toContain('disabled=""');
  });

  it("disables the copy action while another operation is running", () => {
    expect(renderWarning(1, true)).toContain('disabled=""');
  });

  it.each(["zh", "en"] as const)("provides the entire safety workflow in %s", (lang) => {
    const keys = [
      "backup.legacyExportWarning",
      "backup.legacyTokensTitle",
      "backup.legacyTokensWarning",
      "backup.sanitizeHint",
      "backup.sanitizeAction",
      "backup.sanitizing",
      "backup.sanitizedDone",
      "backup.localConfigInfo",
      "backup.localConfigHint",
      "backup.backupRestoreHint",
      "backup.secretsBackupRestoreHint",
    ];
    for (const key of keys) {
      const text = translate(lang, key, { n: 2, path: "sanitized.json" });
      expect(text).not.toBe(key);
      expect(text).not.toMatch(/\{(?:n|path)\}/);
    }
    expect(renderWarning(1, false, lang)).not.toContain("backup.");
  });

  it("describes the absence of an encrypted section without claiming no credentials exist", () => {
    expect(translate("en", "backup.noSecrets")).toBe("No encrypted secrets section");
    expect(translate("zh", "backup.noSecrets")).toBe("不含加密密钥段");
  });

  it("explains manual paired restore for local backups and overwritten credentials", () => {
    const configHint = translate("en", "backup.backupRestoreHint");
    expect(configHint).toContain("quit SynaRoute");
    expect(configHint).toContain("cannot be restored through the import dialog");
    expect(configHint).not.toContain('"Replace"');
    const vaultHint = translate("en", "backup.secretsBackupRestoreHint");
    expect(vaultHint).toContain("overwritten or removed secrets");
    expect(vaultHint).toContain("preserve the current secrets.enc");
  });

  it.each([false, true])("reports plaintext independently of an encrypted section (%s)", (hasSecrets) => {
    const markup = renderToStaticMarkup(createElement(BackupImportPreview, {
      path: "legacy.json",
      preview: {
        formatVersion: 2,
        appVersion: "0.1.81",
        exportedAt: "2026-11-10T00:00:00Z",
        keyCount: 1,
        conflictingKeys: 0,
        suspiciousConflicts: [],
        localOnlyKeys: 0,
        vendorCount: 0,
        brainCount: 0,
        hasSecrets,
        plaintextBalanceTokens: 1,
        localConfig: false,
      },
      busy: false,
      onBusyChange: () => {},
      t: (key, variables) => translate("en", key, variables),
    }));
    expect(markup).toContain("Detected 1 plaintext balance-panel token(s)");
    expect(markup).toContain(hasSecrets ? "Includes an encrypted secrets section" : "No encrypted secrets section");
    expect(markup).toContain("Save a copy without plaintext tokens");
    expect(markup).not.toContain("No secrets included");
  });

  it("distinguishes local rollback files without claiming they are verified portable backups", () => {
    const markup = renderToStaticMarkup(createElement(BackupImportPreview, {
      path: "config.pre-import-legacy.json",
      preview: {
        formatVersion: 2,
        appVersion: "",
        exportedAt: "",
        keyCount: 1,
        conflictingKeys: 0,
        suspiciousConflicts: [],
        localOnlyKeys: 0,
        vendorCount: 0,
        brainCount: 0,
        hasSecrets: false,
        plaintextBalanceTokens: 1,
        localConfig: true,
      },
      busy: false,
      onBusyChange: () => {},
      t: (key, variables) => translate("en", key, variables),
    }));
    expect(markup).toContain("no export checksum");
    expect(markup).toContain("cannot import it as a portable backup");
    expect(markup).toContain("matching vault");
    expect(markup).toContain("Save a copy without plaintext tokens");
    expect(markup).not.toMatch(/backup\.[A-Za-z]/);
  });
});
