import type { Dict } from "./index";

/**
 * English copy.
 *
 * Mirrors every key in `zh.ts`. Same content rules apply: only claims that can be
 * verified against the source, no "100% secure", no invented audits, no fabricated
 * user counts. The repository is public but ships **no license file**, so the wording
 * is consistently "source available", never "open source".
 */
export const en: Dict = {
  // ---------- Common ----------
  "common.download": "Download",
  "common.viewGithub": "View on GitHub",
  "common.docs": "Docs",
  "common.comingSoon": "Coming soon",
  "common.copy": "Copy",
  "common.copied": "Copied",
  "common.close": "Close",
  "common.backHome": "Back to home",
  "common.loading": "Loading…",
  "common.retry": "Retry",
  "common.openInGithub": "View on GitHub",
  "common.skipToContent": "Skip to main content",
  "common.backToTop": "Back to top",
  "common.toggleTheme": "Toggle dark/light mode",
  "common.toggleLang": "Switch language",
  "common.openMenu": "Open menu",
  "common.closeMenu": "Close menu",

  // ---------- Nav ----------
  "nav.home": "Home",
  "nav.brain": "Brain",
  "nav.features": "Features",
  "nav.screenshots": "Screenshots",
  "nav.download": "Download",
  "nav.docs": "Docs",
  "nav.changelog": "Changelog",
  "nav.github": "GitHub",

  // ---------- Hero ----------
  "hero.badge": "Windows desktop app · Runs entirely on your machine",
  // Split in two so Hero can lock the tail with whitespace-nowrap — see zh.ts.
  "hero.titleLead": "When one key fails, ",
  "hero.titleTail": "the next takes over",
  "hero.desc":
    "A local API routing proxy that manages keys from multiple vendors for Claude CLI, the Claude desktop app and the Codex desktop app.",
  "hero.descSecond":
    "When the primary key errors out, the next one takes over — no config edits, no client restart.",
  "hero.ctaPrimary": "Download for Windows",
  "hero.ctaPrimaryMacHint": "macOS build coming soon",
  "hero.ctaSecondary": "View on GitHub",
  "hero.versionPrefix": "Current version",
  "hero.screenshotAlt":
    "SynaRoute main window: the Claude CLI category listing several vendor keys with priority order, health status and the local proxy endpoint",

  // ---------- Benefits ----------
  "benefits.title": "Why you'd want it",
  "benefits.subtitle": "The things multi-key users do by hand every day, handed off to a resident app.",

  "benefits.failover.name": "Automatic failover",
  "benefits.failover.desc":
    "When the primary key errors, times out or gets rate limited, requests move to the next available key in the order you set.",
  "benefits.failover.more":
    "A health probe runs before switching so you don't land on an equally dead key. Keys that keep failing are parked temporarily and rejoin automatically once they recover.",

  "benefits.threeClients.name": "Three clients, kept separate",
  "benefits.threeClients.desc":
    "Claude CLI, the Claude desktop app and the Codex desktop app each get their own category, and their configs never bleed into each other.",
  "benefits.threeClients.more":
    "Every category has its own key list, primary key, proxy port and model mappings. Swapping a key for the CLI leaves the desktop app untouched.",

  "benefits.local.name": "Keys never leave your machine",
  "benefits.local.desc":
    "Config and keys live in local files. No cloud account, no config sync, no usage data collection.",
  "benefits.local.more":
    "Keys are encrypted at rest through the Windows Data Protection API (DPAPI), with an optional master password for a second layer.",

  "benefits.protocol.name": "Protocol translation",
  "benefits.protocol.desc":
    "When your client speaks a different protocol than the vendor expects, the proxy translates in between.",
  "benefits.protocol.more":
    "Anthropic Messages, OpenAI Chat Completions and OpenAI Responses convert both ways, streaming and non-streaming alike.",

  // ---------- Features ----------
  "features.title": "Everything else",
  "features.subtitle": "Built around three problems: many keys, many clients, many protocols.",

  "features.failover.name": "Failover routing",
  "features.failover.short": "Requests land on the next healthy key when the primary one breaks.",
  "features.failover.desc":
    "Keys form a priority list; failover walks it top-down to the first usable one. A health probe runs before switching, and keys that fail repeatedly enter a short circuit-break window where they're skipped, rejoining automatically when it expires. Rate-limit retry hints from upstream are passed through to the client rather than swallowed.",

  "features.mapping.name": "Model mapping",
  "features.mapping.short": "Map a vendor's real model names onto the names your client expects.",
  "features.mapping.desc":
    "Third-party vendors rarely use the model names your client asks for. Add a mapping and the client keeps calling the name it knows while the proxy translates it to the vendor's real one. You can also set a fallback model for when a candidate key doesn't offer the requested one at all.",

  "features.brain.name": "Brain aggregation",
  "features.brain.short": "Several models answer the same question in parallel; a decider writes the final answer.",
  "features.brain.desc":
    "Configure a set of members (a key plus a model) that answer in parallel, then hand their results to a chosen decider model. Summarisation can be compressed or full-context. Members can optionally read files from the working directory for reference, and images can be passed in as input.",

  "features.protocol.name": "Cross-protocol conversion",
  "features.protocol.short": "Any client protocol against any upstream protocol, converted on the fly.",
  "features.protocol.desc":
    "Anthropic Messages, OpenAI Chat Completions and OpenAI Responses convert in both directions, covering streaming, non-streaming, tool calls and multi-turn history. This is also what lets failover cross protocols — your primary and backup keys don't have to be the same vendor.",

  "features.secret.name": "Encrypted key storage",
  "features.secret.short": "Keys are encrypted on disk, with an optional master password.",
  "features.secret.desc":
    "By default keys are encrypted with the Windows Data Protection API (DPAPI), which binds the ciphertext to your current Windows account — copying the file to another machine or account won't decrypt it. You can optionally enable master-password mode, which derives a key from your passphrase with Argon2id and encrypts with AES-GCM; the app then asks you to unlock on startup.",

  "features.apply.name": "One-click client setup",
  "features.apply.short": "Hit Start and the proxy endpoint is written into your client's config.",
  "features.apply.desc":
    "Starting the proxy writes the endpoint into the matching client's config file and stopping it restores the original — with a backup taken before any write. Each of the three clients gets its own fields, so they never overwrite one another. You can preview exactly what will be written before committing.",

  "features.logs.name": "Request logs",
  "features.logs.short": "Every forward is inspectable after the fact.",
  "features.logs.desc":
    "Records when a request was forwarded, which key served it, what the requested model resolved to, what status upstream returned and whether a failover happened. Searchable, with a diagnostic report you can export. Logging full conversation bodies is a separate switch that's off by default.",

  "features.portable.name": "Config import & export",
  "features.portable.short": "Take your whole setup with you; import by merging or replacing.",
  "features.portable.desc":
    "Exports carry a checksum. Because key ciphertext is bound to the local Windows account and won't decrypt elsewhere, exports that include keys are re-encrypted with a passphrase you choose. Machine-local runtime state such as ports and log paths is deliberately left out.",

  "features.tray.name": "Tray & autostart",
  "features.tray.short": "Lives in the tray; start/stop proxies and switch primary keys from there.",
  "features.tray.desc":
    "The tray icon reflects whether proxies are running. Its menu starts and stops each category's proxy and switches the primary key. Optional autostart launches the app minimised to the tray with Windows.",

  // ---------- Brain aggregation spotlight ----------
  // Only claims the app actually delivers. The "decider is required" and "tools are off
  // by default and cost more" notes are deliberate: state the cost up front.
  "brain.badge": "Signature capability",
  "brain.title": "Let several models think it through, then have one decide",
  "brain.subtitle":
    "The same question goes to multiple models in parallel, then a decider model you pick synthesises the final answer. Useful for code review, design work and hard debugging — tasks where a single model tends to miss angles.",

  "brain.flow.members": "Members answer in parallel",
  "brain.flow.membersHint": "2–4 key + model pairs, running at once",
  "brain.flow.merge": "Merge",
  "brain.flow.mergeHint": "Condense to save quota, or pass everything through",
  "brain.flow.decider": "Decider concludes",
  "brain.flow.deciderHint": "Required — pick your strongest model",

  "brain.cap.strategy.title": "Two merge strategies",
  "brain.cap.strategy.desc":
    "Condensed merge has a summariser trim each answer first, which saves quota when you have several members. Full context hands the decider every answer verbatim, which keeps the most information. Concurrency cap and per-member timeout are both adjustable.",
  "brain.cap.retrieval.title": "Reads your code on demand",
  "brain.cap.retrieval.desc":
    "You can enable a set of read-only tools so members decide for themselves which files to read, what to search for and which symbols to look up. The tools never write files or run commands, and stay inside the working directory. Off by default — every round resends the full history, so it costs noticeably more quota.",
  "brain.cap.images.title": "Accepts images",
  "brain.cap.images.desc":
    "Error screenshots and design mockups work as input: up to 4 images, 5MB each. If any image fails validation the whole call errors out and tells you why — it never silently drops one and hands you an answer that didn't actually look at it.",
  "brain.cap.mcp.title": "Also works as an MCP tool",
  "brain.cap.mcp.desc":
    "Turn on the MCP server and Codex CLI or Claude Code can call it directly. That channel only returns suggestions and never touches your files — your client still makes every edit.",

  "brain.ctaDocs": "Read the guide",
  "brain.ctaMcp": "Use it via MCP",
  "brain.screenshotAlt":
    "Brain aggregation settings: member list, final decider, merge strategy, concurrency and timeout",

  // ---------- Screenshots ----------
  "screenshots.title": "A look at the app",
  "screenshots.subtitle":
    "Captured from the app's browser preview mode; all data shown is sample data. Click to enlarge.",
  "screenshots.enlarge": "Click to enlarge",
  "screenshots.placeholder": "Screenshot placeholder (image not available yet)",
  "screenshots.prev": "Previous",
  "screenshots.next": "Next",

  "screenshots.category.title": "Key management",
  "screenshots.category.desc":
    "A card list where you reorder priority, toggle keys on and off, and see health results and the proxy endpoint.",
  "screenshots.brain.title": "Brain aggregation",
  "screenshots.brain.desc": "Configure the members that answer in parallel, the decider model and how results are merged.",
  "screenshots.logs.title": "Request logs",
  "screenshots.logs.desc":
    "Which key served each forward, what the model resolved to and what upstream returned — searchable and expandable.",
  "screenshots.settings.title": "Settings",
  "screenshots.settings.desc":
    "Theme, language, ports, logging and security switches — each one spelling out what it costs to turn on.",
  "screenshots.vendors.title": "Vendor management",
  "screenshots.vendors.desc": "Keep the base URLs and protocol types you use often, ready to pick when adding a key.",

  // ---------- Download ----------
  "download.title": "Download",
  "download.subtitle": "Free to use. No account required.",
  "download.pageTitle": "Download SynaRoute",
  "download.version": "Version",
  "download.minOS": "Requires",
  "download.format": "Package",
  "download.size": "Size",
  "download.button": "Download",
  "download.buttonComingSoon": "Coming soon",
  "download.recommended": "Recommended for you",
  "download.macNote": "The macOS build is in development and is not available for download yet.",
  "download.linuxNote": "No Linux build is planned.",
  "download.fallbackNote":
    "Couldn't fetch the latest release from GitHub, so the known version is shown below. You can also go straight to the releases page.",
  "download.allReleases": "See all releases",
  "download.verifyTitle": "About the install warning",
  "download.verifyDesc":
    "The installer isn't code-signed yet, so Windows SmartScreen may warn about an unknown publisher. If that concerns you, check the file size against the GitHub release page before installing.",
  "download.updateTitle": "About updates",
  "download.updateDesc":
    "The app checks for updates itself and prompts you in-app, so you don't need to come back here to download new versions.",

  // ---------- Steps ----------
  "steps.title": "Four steps to get going",
  "steps.subtitle": "You don't have to read the whole manual first.",

  "steps.s1.title": "Download and install",
  "steps.s1.desc": "Grab the installer and run through it. The local config folder is created on first launch.",
  "steps.s2.title": "Add a vendor key",
  "steps.s2.desc":
    "Pick the client category you're configuring, add a key, and fill in the vendor's base URL and your secret. Saving pulls that vendor's available models and runs one health check.",
  "steps.s3.title": "Hit Start",
  "steps.s3.desc":
    "Starting the local proxy writes its endpoint into the matching client's config file, after backing up the original.",
  "steps.s4.title": "Use your tools as usual",
  "steps.s4.desc":
    "Go back to Claude Code or Codex and work normally. Requests route through the local proxy, and when a key breaks the next one picks up without you noticing.",

  // ---------- Security ----------
  "security.title": "Data & privacy",
  "security.subtitle": "All of this describes what the app actually does; you can check it against the source.",

  "security.storage.title": "Where data lives",
  "security.storage.desc":
    "Entirely on your machine. The config file and the encrypted key file sit in a SynaRoute folder under your user's application data directory. Nothing is stored server-side.",

  "security.encryption.title": "How keys are stored",
  "security.encryption.desc":
    "By default they're encrypted with the Windows Data Protection API (DPAPI), which ties the ciphertext to your current Windows account — copy it to another machine or account and it won't decrypt. Optionally you can enable master-password mode, which derives a key from your passphrase with Argon2id and encrypts with AES-GCM instead.",

  "security.network.title": "What goes out over the network",
  "security.network.desc":
    "Only the upstream vendor requests you configured, plus a check against the GitHub releases page when looking for updates. No usage data, no analytics, no accounts, no config sync.",

  "security.logs.title": "About logging",
  "security.logs.desc":
    "By default logs record metadata only — timestamp, which key served it, the model, the upstream status code. Logging full conversation bodies (including system prompts) is a separate switch that ships off; turn it on only while troubleshooting and turn it back off afterwards. Logs are written locally and nowhere else.",

  "security.delete.title": "Removing your data",
  "security.delete.desc":
    "After uninstalling, delete the SynaRoute folder under your user application data directory to remove all config and keys. The app doesn't keep data anywhere else.",

  "security.source.title": "Source code",
  "security.source.desc":
    "The source is public on GitHub, so everything above is verifiable. Note that the project does not currently ship an open-source license.",

  "security.risk.title": "Risks worth knowing",
  "security.risk.desc":
    "The local proxy listens on a loopback address, so other programs on the same machine can in principle reach that endpoint. Don't expose the proxy port to the internet. Also note that DPAPI protects against the file being copied away — it does not protect against software already running under your account.",

  // ---------- FAQ ----------
  "faq.title": "Frequently asked questions",

  "faq.q1": "Does it cost anything?",
  "faq.a1":
    "No. There's no account to create, no paid tier and no usage cap. You bring your own API keys from whichever vendors you use.",

  "faq.q2": "Which operating systems are supported?",
  "faq.a2":
    "Windows only right now (Windows 10 1809 and later). A macOS build is in development and will be offered here once it's ready. There's no Linux build planned.",

  "faq.q3": "Where are my data and keys stored?",
  "faq.a3":
    "All on your machine, in a SynaRoute folder under your user application data directory. Keys are encrypted at rest and are never uploaded anywhere.",

  "faq.q4": "Do I need to sign in?",
  "faq.a4": "No. There's no account system and no multi-device config sync.",

  "faq.q5": "How do I update?",
  "faq.a5":
    "The app checks for new versions and prompts you in-app, where you can update directly. You can also download a newer installer from the GitHub releases page at any time and install over the top.",

  "faq.q6": "How do I report a problem?",
  "faq.a6":
    "Open an issue on GitHub or email the author. Attaching the file produced by the app's \"export diagnostic report\" makes problems much easier to pin down — that report doesn't contain plaintext keys.",

  "faq.q7": "Is there auto-update?",
  "faq.a7":
    "Yes. The app checks the releases page for newer versions and prompts you; update packages are signature-checked.",

  "faq.q8": "Is it open source?",
  "faq.a8":
    "The source is public on GitHub and you're free to read it and build it yourself. To be precise, though: the project doesn't ship an open-source license yet, so strictly speaking it isn't open-source software.",

  "faq.q9": "Could it get hold of my API keys?",
  "faq.a9":
    "You enter your keys yourself; they're encrypted and stored on your machine and only used to call the vendor endpoints you configured. There's no code path that sends keys to the author or any third party, and you can verify that in the source.",

  "faq.q10": "How is this different from just editing the client config myself?",
  "faq.a10":
    "Editing config by hand points you at one vendor at a time, and when it breaks you have to notice and edit again. SynaRoute puts a local proxy in front so requests hand off between keys automatically, while giving you visual management, health checks and request logs.",

  // ---------- Final CTA ----------
  "cta.title": "Let your keys cover for each other",
  "cta.desc": "Free, entirely local, no account needed.",

  // ---------- Footer ----------
  "footer.tagline":
    "A local API key routing proxy for Claude CLI, the Claude desktop app and the Codex desktop app.",
  "footer.product": "Product",
  "footer.resources": "Resources",
  "footer.legal": "Legal",
  "footer.contact": "Contact",
  "footer.privacy": "Privacy policy",
  "footer.terms": "Terms of use",
  "footer.email": "Email",
  "footer.authorSite": "Author @{name}",
  "footer.copyright": "© {year} SynaRoute. All rights reserved.",
  "footer.sourceNote": "Source available on GitHub (no open-source license attached yet).",

  // ---------- Platforms ----------
  "platform.windows.name": "Windows",
  "platform.windows.format": "exe · NSIS installer",
  "platform.macos.name": "macOS",
  "platform.macos.format": "dmg",
  "platform.linux.name": "Linux",
  "platform.linux.format": "AppImage",

  // ---------- Docs ----------
  "docs.title": "Documentation",
  "docs.subtitle": "Everything from installing to wiring up your clients.",
  "docs.onThisPage": "On this page",
  "docs.backToDocs": "Back to docs",
  "docs.editOnGithub": "View the source document on GitHub",
  "docs.notFound": "That document doesn't exist.",

  "docs.cli.title": "Claude CLI setup guide",
  "docs.cli.desc":
    "Install to working, end to end — what gets written to your config, how models are picked, and common problems.",
  "docs.brain.title": "Brain aggregation guide",
  "docs.brain.desc":
    "Set up several models answering in parallel, summarisation and the final decider, with what each setting means.",
  "docs.mcp.title": "MCP setup guide",
  "docs.mcp.desc": "Expose brain aggregation as an MCP tool for Codex CLI and Claude Code.",

  // ---------- Changelog ----------
  "changelog.title": "Changelog",
  "changelog.subtitle": "Release notes pulled from the GitHub releases page.",
  "changelog.loadFailed": "Couldn't load the changelog from GitHub.",
  "changelog.loadFailedHint":
    "This is usually a network issue or API rate limiting. You can read the releases page directly instead.",
  "changelog.empty": "No releases yet.",
  "changelog.viewOnGithub": "View this release on GitHub",

  // ---------- Privacy ----------
  "privacy.title": "Privacy policy",
  "privacy.updated": "Last updated: {date}",

  // ---------- Terms ----------
  "terms.title": "Terms of use",
  "terms.updated": "Last updated: {date}",

  // ---------- 404 ----------
  "notFound.title": "Page not found",
  "notFound.desc": "There's no page at that address — the link may be out of date or mistyped.",
};
