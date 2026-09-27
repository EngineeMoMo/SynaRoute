# Privacy policy

This policy describes what information the SynaRoute application and this website each handle.

## 1. The SynaRoute application

### No personal information is collected

SynaRoute is a desktop application that runs entirely on your own machine. It has **no account system, requires no registration or sign-in, performs no config sync, and collects no usage data or analytics**. There is no server of ours receiving anything from you.

### Where data is stored

Config (config.json) and keys (secrets.enc) live in the user data directory: usually %APPDATA%\SynaRoute on Windows, ~/Library/Application Support/SynaRoute on macOS, and $XDG_DATA_HOME/SynaRoute (or ~/.local/share/SynaRoute) on Linux. Logs may be elsewhere; check the actual path in Settings.

SYNAROUTE_DATA_DIR overrides the config and vault directory when set; use the actual configured path. Keys are stored locally, not uploaded to a SynaRoute cloud; upstream requests use them to authenticate to your configured endpoints.

- `config.json` — your key list, model mappings and settings;
- `secrets.enc` — your encrypted API keys.

Logs contain metadata by default; enabling conversation logging includes message bodies and system prompts, so use it only temporarily for troubleshooting. Windows/Linux prefer logs beside the executable, falling back to SynaRoute/logs in the user data directory if unwritable. macOS defaults to ~/Library/Logs/SynaRoute. Custom log directories are supported; check Settings for the active path.

### How keys are protected

Windows defaults to account-bound DPAPI; macOS keeps a random encryption key in Keychain. Both can switch to master-password mode. Linux requires a master password and has no fixed-key fallback. Master-password mode uses Argon2id + AES-256-GCM and requires unlocking each launch; forgotten passwords cannot recover keys.

Legacy Linux fixed-key vaults are not automatically migrated or overwritten. Back up before upgrading. If a legacy vault is rejected, quit, move secrets.enc out of the data directory, restart, set a master password and re-enter upstream keys. Rotate old keys and securely remove old backups after verifying the new vault.

To be clear about what this protects against: encryption guards against the file being copied away. It does **not** guard against software already running under your own account.

### What the application sends over the network

Two things only:

1. The upstream vendor requests you configured — the AI calls being proxied, sent to the addresses you entered on your keys;
2. A request to the project's releases page when checking for updates.

Nothing else leaves your machine.

### About logging

Request logs record metadata only by default: the timestamp, which key served the request, what the requested model resolved to, the upstream status code and whether a failover occurred.

Settings contains a "Log model calls" switch that is **off by default**. Turning it on additionally records the **full conversation text, including system prompts**. Enable it only while troubleshooting and turn it back off afterwards. All logs are written locally and are never transmitted.

### Deleting your data

Before uninstalling, stop the proxy and restore client configuration. Note the data and log directories in Settings, then quit. Remove the SynaRoute data folder (including config, vault and backups), the active log directory, and any previously used custom log directories. Separately remove exported configs, diagnostic reports and client config backups from their saved locations. Deleting only the data directory is not a complete cleanup.

## 2. This website

This is a static site hosted on GitHub Pages.

- The site itself **sets no tracking cookies** and loads no third-party advertising or analytics scripts;
- Your theme and language choices are kept in your browser's local storage purely so they persist on your next visit; they are not sent anywhere;
- The download page queries GitHub's public API for the latest release, in order to show the version number and download links;
- As the host, GitHub may log requests (such as IP addresses) under its own policies. That is outside our control — see GitHub's privacy statement for details.

## 3. Third-party services

Every upstream vendor you configure in SynaRoute is an **independent third party**. Your request content is sent to them, and how they handle it is governed by their own terms. Please read the privacy policy of whichever vendors you use.

## 4. Changes

If this policy changes, this page will be updated.
