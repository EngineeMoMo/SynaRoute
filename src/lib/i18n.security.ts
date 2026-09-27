type Dict = Record<string, string>;

export const securityZh: Dict = {
  "settings.security": "安全与加密",
  "settings.masterPwTitle": "启用主口令增强",
  "settings.masterPwRequired": "Linux 必须先启用主口令才能保存或使用密钥，启用后不能关闭（可修改）。每次启动需解锁；忘记口令无法恢复密钥。",
  "settings.masterPwDesc": "Windows 默认使用 DPAPI；macOS 使用系统钥匙串保护。启用主口令后，改用 Argon2id + AES-256-GCM 加密密钥库，每次启动需解锁。",
};

export const securityEn: Dict = {
  "settings.security": "Security & Encryption",
  "settings.masterPwTitle": "Enable master password",
  "settings.masterPwRequired": "Linux requires a master password before saving or using keys. It cannot be disabled, but can be changed. Unlock each launch; a forgotten password cannot recover your keys.",
  "settings.masterPwDesc": "Windows uses DPAPI; macOS uses Keychain protection. A master password replaces system protection with Argon2id + AES-256-GCM; unlock once per launch.",
};
