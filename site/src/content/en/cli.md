# Claude CLI setup guide

For users of the Claude Code command line (`claude`). SynaRoute runs a proxy on your machine that routes `claude`'s requests across the keys you've configured, with failover and model mapping — when one key stops working, the next one takes over.

## What it solves

You have API keys from several AI services (different vendors, different relays), and you want the `claude` CLI to:

- **switch automatically to the next key** when one is rate-limited or down, without editing config;
- **map model names automatically** when the vendor doesn't use the names your client asks for (the client wants `claude-opus-*`, the upstream only knows something else);
- keep your keys **encrypted locally** instead of scattered across plaintext config files.

SynaRoute opens a local proxy port on `127.0.0.1`. `claude` sends requests there, and SynaRoute forwards them upstream according to your rules. **It listens on loopback only and is not exposed externally.**

## Installing

1. Get the installer from the [download page](/en/download) and run it (per-user install, no administrator rights needed).
2. If WebView2 is missing on first run, the installer downloads it automatically (Windows 11 usually ships with it).
3. Launch **SynaRoute** from the Start menu.

Uninstalling works like any other app: Control Panel → Programs → Uninstall.

> **Requirement**: Claude Code CLI **v2.1.129 or newer**. On older versions the `/model` picker won't fetch the model list the proxy exposes. Check with:
>
> ```bash
> claude --version
> ```

## Three steps to get running

### Step 1: Add a key

Open SynaRoute, go to the "Claude CLI" category, click "Add key" and fill in:

| Field | What to put |
|---|---|
| Name | Anything you'll recognise, e.g. `primary`, `backup` |
| Vendor / protocol | The upstream's type (native Anthropic / OpenAI-compatible / custom) |
| Base URL | The upstream address, e.g. `https://api.your-provider.com` |
| API key | Your upstream secret. Stored encrypted locally, never written to a plaintext config |
| Models | Can be left empty (fetched automatically on save), or typed in manually |

For failover you want **more than one key**. Drag them up and down to set priority — higher in the list means used earlier.

### Step 2: Hit Start

Click **Start** in the status bar at the top. Two things happen:

1. A proxy port opens on `127.0.0.1`;
2. `~/.claude/settings.json` is **written automatically** (the original is backed up first) to point `claude` at that proxy.

The status bar then shows the running state, the current address `http://127.0.0.1:<port>`, and the routing mode (direct for a single key, failover with a count for several). There's a copy button next to the address.

You do **not** need to edit any config file by hand — Start has already wired `claude` up.

### Step 3: Use claude as usual

**Open a new terminal** so it picks up the config that was just written, then work normally:

```bash
claude
```

`claude` now goes through the SynaRoute proxy, and `/model` lists the models SynaRoute exposes.

## About starting and stopping

- **The proxy doesn't start with the app.** Click Start in SynaRoute each time you want to use it.
- **Closing the window isn't quitting.** The close button hides the app to the system tray; the proxy keeps running in the background and `claude` keeps working. To stop it completely: right-click the tray icon → Quit.
- **The port is dynamic.** It may change between starts, but starting writes the new port back into `~/.claude/settings.json`, so a fresh terminal is all you need.

## What gets written to your config

After you click Start, SynaRoute writes these fields into `~/.claude/settings.json` (**backing up the original first**):

| Field | Value | Purpose |
|---|---|---|
| `env.ANTHROPIC_BASE_URL` | `http://127.0.0.1:<port>` | Points `claude` at the local proxy |
| `env.ANTHROPIC_AUTH_TOKEN` | A placeholder | The proxy doesn't check it, but the CLI requires the field to exist |
| `env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` | `1` | Makes `/model` fetch the model list from the proxy |
| `env.ANTHROPIC_MODEL` and top-level `model` | The primary key's default public model name | The default entry in `/model` |

Any legacy `ANTHROPIC_DEFAULT_HAIKU/SONNET/OPUS_MODEL` entries are **removed** if present, so `/model` doesn't end up showing three identically named items. The three tiers (Haiku / Sonnet / Opus) are resolved inside the proxy instead of being hard-coded in the client.

To see exactly what is currently written, use the "Config preview" button in the status bar (read-only, with the secret redacted).

## How a model gets picked

`claude` sends a model name (say `claude-opus-4-x`), and SynaRoute decides what to actually send upstream in this order, **highest priority first**:

1. **Exact mapping** — you configured a "client name → real upstream name" mapping for that key. Your explicit intent always wins.
2. **Tier match** — the requested name contains `haiku` / `sonnet` / `opus` and you set that tier for the key, so that tier's model is used.
3. **Native same name** — the key's model list already contains exactly that name, so it's used as-is.
4. **Default model** — falls back to the default model configured on that key.
5. **First in the list** — if none of the above apply, the first model in the key's list.
6. **Pass-through** — the key has no models configured at all, so the requested name goes upstream unchanged.

> **A gotcha worth knowing**: tier matching ranks **above** "native same name". If you set "tier Opus = some model" on a key, then **every** request containing `opus` is rewritten to that model — even when the key natively supports the name the client asked for. If you don't want that rewrite, leave the tier empty.

Model names that don't start with `claude` or `anthropic` (say `grok-4.5`) get silently filtered out by the CLI. SynaRoute automatically wraps them as `claude-synaroute-<original>` when exposing them to `/model` and strips the prefix again when forwarding. So seeing `claude-synaroute-xxx` in `/model` is expected.

## Failover and health status

- **Failover happens automatically with several keys.** When the primary returns rate limiting, busy or an error (HTTP 429 / 5xx and friends), the next candidate key is tried, transparently to `claude`. The request log shows the failover.
- **Health checks** probe each key's reachability on an interval (60 seconds by default) and show status and latency in the list.
- **Health status only affects what's displayed — it does not gate routing.** Keys showing as unreachable are still tried by real traffic; only repeated real failures trigger a short circuit-break window where the key is skipped. Occasional red status doesn't mean the key is unusable.

Two more switches live in Settings → Debug:

- **Log model calls** — records "what the client asked for → what was actually sent → the result" on every forward. Off by default, because turning it on makes logs contain full conversation text.
- **Use a real request for health checks** — the probe sends a tiny real request, which is closer to actual traffic. The cost is that rate-limited or busy keys then show as unreachable, which looks alarming but doesn't affect forwarding. Turn it off for fewer red markers and a lightweight connectivity probe instead.

## Common problems

**`/model` doesn't show my models.**

Check, in order: CLI version ≥ v2.1.129; you've clicked Start in SynaRoute; the key has models configured (or fetched successfully); and you're running `claude` in a terminal **opened after** you clicked Start.

**`claude` says connection refused.**

The proxy isn't running. Go back to SynaRoute, click Start, then open a new terminal. Note that closing the window only hides it to the tray — but if you chose Quit from the tray, you'll need to start it again.

**A model keeps returning 502 / 503 / 429.**

That comes from **upstream**, not from SynaRoute. Check whether that key's upstream is overloaded, rate-limiting you or has an account problem. Configuring more keys lets failover cover for it.

**A request got rewritten to a model I didn't expect.**

Most likely one of that key's tiers has a value set — see the gotcha under "How a model gets picked". Leave the tier you don't want empty.

**I want to bypass SynaRoute and go direct for a while.**

SynaRoute backed up your `~/.claude/settings.json` before changing it, so you can restore from that backup, or just delete the `ANTHROPIC_BASE_URL` and related fields by hand. Stopping the proxy also restores the original automatically.

**Are my keys safe?**

Keys are encrypted and stored locally in `secrets.enc`; they are never written into `settings.json`, which only holds a placeholder token. The config preview redacts them too.

## Updating

SynaRoute checks for updates itself: when a new version exists an indicator appears in the sidebar, and Settings has a one-click check-and-install. You can also download a new installer and install over the top.

## Reporting problems

When reporting an issue, please include:

1. What you saw — the exact error text from `claude`, or a screenshot;
2. The relevant lines from SynaRoute's request log, or just use "Export diagnostic report" in Settings;
3. Which key (the name is enough) and which model name were involved.

Please do **not** paste real API keys. Logs and the config preview are redacted, but take care when copying things manually.
