# MCP setup guide

How to call SynaRoute's multi-model brain aggregation from MCP-capable clients such as Codex CLI and Claude Code.

## What this is

SynaRoute ships an MCP (Model Context Protocol) server. Once enabled, MCP-capable AI coding clients can call multi-model aggregation directly:

- several models analyse your current project's code **in parallel**;
- a **decider** weighs all of it and produces a clear recommendation or change plan;
- the recommendation goes back to your client, and **your client makes the file edits** using its own editing tools and approval flow.

**SynaRoute only advises; it doesn't touch your files.** Every actual change goes through your confirmation in the client.

## Prerequisites

1. SynaRoute is installed and running;
2. At least one category (Claude CLI / Claude desktop / Codex) has working keys, with aggregation enabled and members plus a decider configured on the "Brain aggregation" page;
3. Your client is installed (a Codex CLI version supporting HTTP MCP, or Claude Code).

## Enabling the MCP server

1. Open SynaRoute → **Settings** → **MCP server**;
2. Turn on "Enable MCP server";
3. The default port is **9527**. If it's taken by a system process (on some Windows machines 9527 / 9528 are held by driver or fingerprint services), SynaRoute searches upward for a free port;
4. The "Service address" on the card shows the **port actually bound** — copy that address for your client config;
5. A green indicator means the service is running; if it's red, hover to see why.

> Because the port can shift automatically, **always copy the real address from the settings page** rather than assuming 9527.

## Connecting Codex CLI

Edit `~/.codex/config.toml` (on Windows, `C:\Users\<you>\.codex\config.toml`) and add:

```toml
[mcp_servers.synaroute]
url = "http://127.0.0.1:9527/mcp"   # use the real address from the settings page
```

Optional: add a line to your project's `AGENTS.md` so Codex reaches for it at the right moments:

```markdown
## Multi-model collaboration

For complex code review, architecture design or hard debugging, prefer calling the
synaroute_ai tool, passing the current project's absolute path as cwd, and read the
combined analysis before making changes.
```

After that just ask normally in Codex, e.g. "use synaroute to review the security of the src/auth module".

## Connecting Claude Code

Add this to your project's `.claude/settings.json` (or the user-level `~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "synaroute": {
      "url": "http://127.0.0.1:9527/mcp"
    }
  }
}
```

Again, use the real address shown on the settings page.

Optional: configure a hook so prompts containing "review" or "refactor" nudge toward multi-model analysis:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "matcher": "review|refactor",
        "hooks": [
          {
            "type": "prompt",
            "prompt": "Prefer calling synaroute_ai for multi-model analysis, passing the project's absolute path as cwd"
          }
        ]
      }
    ]
  }
}
```

## Tool parameters

`synaroute_ai` accepts:

| Parameter | Required | Description |
|---|---|---|
| `prompt` | Yes | The task, e.g. "review the security of the auth module" |
| `cwd` | No | Absolute path to the current project root. **Strongly recommended** — without it, the most recently active project detected by auto-follow is used |
| `category` | No | Which category's key pool and aggregation config to use. Defaults to `claude-cli`; also `claude-desktop` or `codex` |
| `languageHint` | No | Answer language, e.g. `zh` / `en`. Omit to follow the prompt |
| `images` | No | Images to look at, as paths **relative to `cwd`**. Requires `cwd` to be passed as well |

### Image input

For tasks like "what's wrong in this error screenshot". The limits, and what happens when they're not met:

- At most **4 images**, each **under 5 MB**, in `png` / `jpg` / `jpeg` / `gif` / `webp` — the four formats the Anthropic and OpenAI protocols **both** support.
- Paths must stay inside the working directory. `..`, absolute paths, drive letters or UNC paths, and symlinks pointing outside the working directory are all refused.
- Credential-like filenames (`.env*`, keys, certificates) are always refused; the image path goes through that same guard.
- **If any one of these fails, the whole call errors with an explanation — no image is ever dropped silently.** Dropping one quietly would hand you an answer that looks fine but was produced without ever seeing the picture.
- Member models must support image input. Text-only models get rejected upstream, and the failure reason notes that.

### On-demand retrieval by members

**Off by default**; enable it per category under "Brain aggregation → Tool calls". Once on, members can use a set of **read-only** tools to decide what to look at: read a file (optionally a line range), regex search, list a directory, query a symbol index.

- Needs a working directory: from this tool's `cwd`, or from auto-follow / a directory set in the desktop app. With none of those, no tools are offered that round and the request log records why.
- The tools are **read-only** — they never write files or run commands, they're confined to the working directory, and credential-like files are always refused.
- There's a round limit (6 by default, adjustable 2–12). Once reached, the model is asked to conclude from what it already has.
- **It noticeably increases token usage**: every tool-call round resends the entire conversation history.

## What comes back

The tool returns Markdown containing: the combined analysis or change plan, the list of participating models, the decider, how many files were retrieved, elapsed time, and a reminder to apply changes with your own editing tools.

## Multiple projects

Each tool call carries its own `cwd` and becomes an independent aggregation run. Project A and project B aggregate concurrently without interfering, and member and decider contexts are fully isolated.

## Logging

MCP calls are recorded on the app's "Request logs" page (tagged `mcp`) and in the daily log files in your log directory. Each entry records the time, working directory, member count, retrieved file count, elapsed time and success or failure.

## Common problems

**The indicator is red, or my client can't connect.**

Confirm the switch is on and the indicator is green; **use the real address from the settings page** (the port may not be 9527); hover the red indicator for the reason. If it reports that all ports are taken, set a port manually in Settings.

**It returns "brain aggregation is not enabled for this category".**

Go to the "Brain aggregation" page and enable aggregation for that `category`, with members and a decider configured.

**It returns "no final decider configured".**

The brain aggregation page requires a decider to work.

**Will SynaRoute edit my files?**

No. The MCP channel only returns advice; every file change is performed by your client and confirmed by you.

**Is it secure?**

The MCP server listens on `127.0.0.1` only and isn't exposed to the internet, so it doesn't authenticate. That does mean other local processes on the same machine could in principle call it — an acceptable trade-off for single-machine personal use, but don't forward that port to the internet.

## Known boundaries

- **File retrieval** prefers `ripgrep` when installed (fastest); otherwise it falls back to a built-in walk that works the same but is slower. That fallback has cost ceilings (max depth, a file-count cap, common text extensions only, skipping `node_modules`, `target`, `dist`, `.git` and similar), so very large repositories may miss deeply nested files.
- **Symbol-index retrieval** only applies when the target project already has an index and the corresponding CLI is installed; otherwise it's skipped without affecting other retrieval.
- **Auto-following the working directory** supports Claude CLI and Codex session history. The Claude desktop app doesn't persist a resolvable project path, so auto-follow usually can't work there — pass `cwd` explicitly.

## Turning it off

Settings → MCP server → turn the switch off. The service stops immediately and won't start again next launch.
