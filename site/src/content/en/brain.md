# Brain aggregation guide

Several models analyse the same request in parallel; their answers are merged and a "decider" produces the final result. It suits code review, design work and hard debugging — tasks where more than one perspective helps.

There are **two ways to use it**:

| Channel | Entry point | Who triggers it | Does it touch files |
|---|---|---|---|
| Desktop panel | The app's "Brain aggregation" page | You type a request and hit run | Not while producing the plan; only after you confirm execution |
| MCP tool | Codex / Claude Code calling `synaroute_ai` | A tool call inside your client | **Never touches files** — it returns advice and your client makes the edits |

This page covers the desktop panel. For the MCP channel see the [MCP setup guide](/en/docs/mcp).

## Prerequisites

1. **At least two working keys in the same category**, each with a valid secret and healthy status. Both members and the decider are chosen from those keys, so set them up first.
2. Open the "Brain aggregation" page in the left sidebar. Note it has **its own category switcher** at the top (Claude CLI / Claude desktop / Codex), and the three categories keep **independent** aggregation configs — make sure you're on the right one.

## Configuration, top to bottom

### Enable brain aggregation

Turn on the switch at the top. Without it, the run panel below won't appear.

### Members

These are the models that think in parallel; two to four is plenty.

- Click "Add member" → pick a **key** from the dropdown → then pick one of that key's **models** (the models you fetched or typed in earlier in the key editor).
- Added members are listed with a brand icon and a health marker, and can be removed.
- **Suggestion**: add two members from different vendors or model families — that's where the multi-perspective effect actually shows.

### Decider and summarisation

- **Final decider** (required): the model that weighs all member answers and produces the final result. Pick a key plus a model. Aggregation can't be enabled without one. A more capable model is a good choice here.
- **Summariser** (optional): only used when the strategy is "compressed summary"; leave it empty to reuse the decider.

### Aggregation strategy

- **Compressed summary**: a summariser condenses the member answers before the decider sees them. **Cheaper in tokens** — use it when you have many members or large context.
- **Full context**: member answers go to the decider verbatim. **Most complete** — use it when you have few members. For a first try this is the clearer option.
- **Concurrency limit**: how many members run at once, 3 by default.
- **Timeout**: the timeout for a **single member's** call, 60000 ms by default. This is a per-member ceiling rather than a whole-round one, so one slow member won't drag down the ones that already finished.

### File retrieval (optional)

When enabled, relevant files from the working directory are retrieved and injected into context so the models can actually "see" your code.

- **Follow the most recent project automatically**: on, it picks up the project directory you most recently opened in Claude CLI or Codex (the detected directory is shown); off, you fill in the working directory yourself or pick one from "recent".
- **Retrieved content token limit**: the trim ceiling for injected context, 50000 by default.

> For a first try, turn the auto-follow off and point it at a **small project** — fewer files, faster retrieval, easier to observe what happens.

### Tool calls (optional, off by default)

When enabled, members can decide for themselves which files to look at using a set of **read-only** tools: read a file (optionally a line range), regex search, list a directory, query a symbol index. That's more accurate than guessing at keywords up front.

**The cost is noticeably higher token usage** — every tool-call round resends the whole conversation history. That's why it ships off.

The tools are always read-only: they never write files or run commands, they're confined to the working directory, and credential-like files are always refused.

### Save

Click "Save aggregation config". A "saved" confirmation means it hit disk. A failed write raises a visible error rather than failing silently.

## Running it

Once the switch is on and a decider is configured, a "Run aggregation" panel appears below the settings:

1. Type a request, e.g. "review this project's error handling for robustness and point out the files that need changing".
2. Click Start. Members run in parallel, results are merged, and the decider produces a plan. This **really consumes your key quota** and takes anywhere from seconds to tens of seconds depending on the models.
3. The decider's plan appears in the "Plan" area below.
4. If retrieval is on and the plan names files to change, you can click "Confirm execution": the decider writes out the full file contents per the plan into the working directory, and the results area lists what changed. Click Cancel if you don't want files touched.

As a safety measure, a decider that tries to write to `..` or an absolute path is refused — it can't escape the working directory.

## Image input

Images can be passed in through the MCP channel, for tasks like "what's wrong in this error screenshot". See the [MCP setup guide](/en/docs/mcp) for the limits.

Member models must support image input. Text-only models get rejected upstream, and the failure reason says so.

## Symptoms and fixes

| What you see | Why | What to do |
|---|---|---|
| Nothing happens on run, or the panel never appears | Aggregation isn't enabled, or no decider is configured | Turn the switch on and configure a decider |
| Stuck "thinking" for a long time | Slow member or decider models, or the timeout is set very high | Wait, or lower the timeout |
| Only the decider's view comes back; members seem absent | Every member key failed or timed out, so it degraded to the decider answering alone | Check the member keys' health, secrets and model names |
| An error mentioning the decider | The decider's key is unusable | Switch to a working decider key |
| Retrieval is on but the model says it sees no files | Wrong working directory, or no relevant files there | Check the path; installing `ripgrep` makes retrieval faster |

## About fault tolerance

- **A failing member is skipped silently.** If one member's key is unusable, that member drops out and the rest still produce results.
- **A failing decider is a hard error.** The decider is required, so if it's unavailable the whole run fails rather than pretending to succeed.
