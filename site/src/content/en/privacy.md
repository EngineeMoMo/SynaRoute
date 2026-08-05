# Privacy policy

This policy describes what information the SynaRoute application and this website each handle.

## 1. The SynaRoute application

### No personal information is collected

SynaRoute is a desktop application that runs entirely on your own machine. It has **no account system, requires no registration or sign-in, performs no config sync, and collects no usage data or analytics**. There is no server of ours receiving anything from you.

### Where data is stored

All configuration and keys live in a `SynaRoute` folder under your user's application data directory on your computer:

- `config.json` — your key list, model mappings and settings;
- `secrets.enc` — your encrypted API keys.

Log files are written to a `logs` folder next to the application by default, and can be pointed elsewhere in Settings.

### How keys are protected

By default they are encrypted at rest with the Windows Data Protection API (DPAPI). The ciphertext is bound to your current Windows account, so copying the file to another machine or account won't decrypt it.

You can also enable master-password mode in Settings, which derives a key from your passphrase with Argon2id and encrypts with AES-GCM instead. With it enabled you unlock on each launch.

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

After uninstalling, delete the `SynaRoute` folder under your application data directory to remove all configuration and keys. If you changed the log directory, delete that too. The application keeps no data anywhere else.

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
