type Dict = Record<string, string>;

export const backupSecurityZh: Dict = {
  "backup.legacyExportWarning": "旧版备份即使选择了「不含密钥」，也可能含余额面板 Token 明文。升级或重新导出不会清理旧文件；可在导入预览中检测并另存脱敏副本，若旧文件曾对外分享，应撤销或轮换 Token。",
  "backup.hasSecrets": "含加密的密钥段（需口令）",
  "backup.noSecrets": "不含加密密钥段",
  "backup.legacyTokensTitle": "检测到 {n} 个明文余额面板 Token",
  "backup.legacyTokensWarning": "此旧备份含敏感凭证，不可按「不含密钥」文件分享。升级或导入不会清理原文件；若曾对外分享，应撤销或轮换相关 Token。",
  "backup.sanitizeHint": "另存副本只移除旧版明文面板 Token，保留其他配置、凭证引用和加密密钥段。导出备份重新计算校验和，本机回滚配置保持原格式。不需导入配置或解锁密钥库，不覆盖原文件或已有文件。",
  "backup.localConfigInfo": "本机回滚配置（无导出校验和）",
  "backup.localConfigHint": "这是 config.json 格式的本机回滚副本，只能在此检测并另存脱敏副本，不能作为可移植备份导入。恢复需退出应用后，配合对应密钥库手动还原；副本仍可能含其他敏感配置，请勿直接分享。",
  "backup.backupRestoreHint": "如需回滚，请先退出 SynaRoute 并另存当前 config.json，再将此本机回滚备份还原为数据目录中的 config.json；它不能通过「导入配置」恢复。若下方提供密钥库备份，请一并配套还原。",
  "backup.secretsBackupRestoreHint": "此密钥库快照保留导入前的凭证，包括被覆盖或删除的密钥。请在 SynaRoute 关闭时，另存当前 secrets.enc，再将这份配套备份还原为 secrets.enc。还原后仍需对应主口令或原系统账户。",
  "backup.sanitizeAction": "另存已移除明文 Token 的副本",
  "backup.sanitizing": "另存脱敏副本中…",
  "backup.sanitizedDone": "已移除 {n} 个明文面板 Token，副本保存至 {path}。原文件和本机配置未改动，原文件仍须按敏感文件处理；副本不再含这些明文 Token，相关余额查询可能需要重新配置凭证。",
};

export const backupSecurityEn: Dict = {
  "backup.legacyExportWarning": "Older backups may contain plaintext balance-panel tokens even when exported without secrets. Upgrading or exporting again does not clean old files. Use the import preview to detect them and save a sanitized copy; revoke or rotate tokens if the original was shared.",
  "backup.hasSecrets": "Includes an encrypted secrets section (password required)",
  "backup.noSecrets": "No encrypted secrets section",
  "backup.legacyTokensTitle": "Detected {n} plaintext balance-panel token(s)",
  "backup.legacyTokensWarning": "This legacy backup contains sensitive credentials and must not be shared as a secrets-free file. Upgrading or importing does not clean the original file. Revoke or rotate the tokens if it was shared.",
  "backup.sanitizeHint": "Save a copy with only legacy plaintext panel tokens removed. Other settings, credential references and encrypted secrets are preserved. Export checksums are recalculated; local rollback files keep their original format. No import or vault unlock is needed; original and existing files are never overwritten.",
  "backup.localConfigInfo": "Local rollback configuration (no export checksum)",
  "backup.localConfigHint": "This is a local config.json rollback copy. You can inspect it and save a sanitized copy here, but cannot import it as a portable backup. To restore it, exit the app and manually restore it with the matching vault. Other sensitive settings may remain, so do not share it directly.",
  "backup.backupRestoreHint": "To roll back, quit SynaRoute first and preserve the current config.json. Restore this local rollback file as config.json in the data directory; it cannot be restored through the import dialog. Restore the matching vault backup too, if provided.",
  "backup.secretsBackupRestoreHint": "This vault snapshot preserves credentials from before the import, including overwritten or removed secrets. With SynaRoute closed, preserve the current secrets.enc and restore this matching snapshot as secrets.enc. The original master password or OS account is still required.",
  "backup.sanitizeAction": "Save a copy without plaintext tokens",
  "backup.sanitizing": "Saving sanitized copy…",
  "backup.sanitizedDone": "Removed {n} plaintext panel token(s) and saved a copy to {path}. The original file and local settings are unchanged; the original still needs sensitive handling. The copy no longer contains those plaintext tokens, so balance queries may require credentials to be configured again.",
};
