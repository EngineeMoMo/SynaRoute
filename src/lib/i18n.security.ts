type Dict = Record<string, string>;

export const securityZh: Dict = {
  "settings.lanPartial": "部分操作失败，已同步实际设置与代理状态。",
  "settings.lanUnknown": "无法确认局域网访问状态，请重试关闭或检查代理。",
  "settings.lanCloseRetry": "重试关闭局域网访问",
  "settings.lanRunning": "运行中",
  "settings.lanStopped": "已停止",
  "settings.security": "安全与加密",
  "settings.masterPwTitle": "启用主口令增强",
  "settings.masterPwRequired": "Linux 必须先启用主口令才能保存或使用密钥，启用后不能关闭（可修改）。每次启动需解锁；忘记口令无法恢复密钥。",
  "settings.masterPwDesc": "Windows 默认使用 DPAPI；macOS 使用系统钥匙串保护。启用主口令后，改用 Argon2id + AES-256-GCM 加密密钥库，每次启动需解锁。",
};

export const securityEn: Dict = {
  "settings.lanPartial": "Some operations failed. Actual settings and proxy states are shown.",
  "settings.lanUnknown": "LAN access state is unknown. Retry disabling it or check the proxies.",
  "settings.lanCloseRetry": "Retry disabling LAN access",
  "settings.lanRunning": "Running",
  "settings.lanStopped": "Stopped",
  "settings.security": "Security & Encryption",
  "settings.masterPwTitle": "Enable master password",
  "settings.masterPwRequired": "Linux requires a master password before saving or using keys. It cannot be disabled, but can be changed. Unlock each launch; a forgotten password cannot recover your keys.",
  "settings.masterPwDesc": "Windows uses DPAPI; macOS uses Keychain protection. A master password replaces system protection with Argon2id + AES-256-GCM; unlock once per launch.",
};
