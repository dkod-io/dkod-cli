# Blog post draft — "dkod speaks Agent Trace"

> **WHEN TO SEND / WHERE:** dkod.io blog, published the SAME WEEK as Show HN (weeks 3–4). Export has shipped (`dkod export agent-trace`, spec v0.1.0 — see `docs/agent-trace.md`). Syndicate: link from the HN maker comment if live, X/Bluesky thread, lobste.rs. NEEDS HUMAN TO SEND (publish + syndication).

## Title

```
dkod speaks Agent Trace
```

## Post body

```markdown
As of v0.X.X, dkod exports [Agent Trace](https://github.com/cursor/agent-trace) —
the open attribution format for AI-generated code (spec v0.1.0). `dkod export
agent-trace` emits a standard trace record from any captured session — one
JSON file per session, or a single record to stdout. Import — ingesting Agent
Trace records produced by other tools into `refs/dkod/*` — is next on the
roadmap.

This post is about why we adopted someone else's format instead of pushing
our own.

## What Agent Trace is

Agent Trace is a storage-agnostic specification for recording which AI
agent contributed which code, started under the `cursor` GitHub org with
participation from Cursor, Cognition, Cloudflare, Vercel, and git-ai. It
defines the *shape* of an attribution record — agent, model, conversation
reference, file ranges — and deliberately does not define where records
live. That's the right call for a spec: Cursor wants them in its own
backend, git-ai keeps them in git notes with prompts stored elsewhere,
CI vendors want them in their pipelines.

A format that many tools write and no tool owns is how attribution becomes
infrastructure instead of a feature. The commit-trailer era — `Co-authored-by:
Claude` and friends — proved both halves of this: even a one-line convention
got used industry-wide because it was neutral, and it captured far too
little to answer real questions.

## "Storage-agnostic" still needs storage

A spec that doesn't define storage leaves the most important operational
question to the ecosystem: where do trace records live such that they are
durable, shared across a team, access-controlled, and attached to the code
they describe — for years?

We think that question was answered in 2005. Git already provides
content-addressed durability, distribution, and the access-control model
your organization has already audited. dkod's whole architecture is that
agent sessions belong in the repository, as git objects under
`refs/dkod/*`, on your own git host. Transcripts are never stored outside
your git host.

So dkod's position in the Agent Trace ecosystem is the natural one:

**dkod is the git-native Agent Trace store.**

- Anything dkod captures (7 agents today: Claude Code, Codex, Copilot CLI,
  Cursor, Factory droid, Gemini CLI, opencode) can flow OUT as Agent Trace
  via `dkod export agent-trace` — into dashboards, CI checks, review tools,
  whatever speaks the format. No export lock-in.
- The other direction — preserving trace records other tools emit in-repo,
  next to the full session transcript, via a `dkod import` — is the next
  interop milestone.

## Trace records point; sessions answer

One distinction worth keeping crisp: an Agent Trace record is a pointer —
*this file, this agent, this session* (line ranges too, for tools that
track them; dkod records file + commit granularity and honestly emits empty
ranges rather than fabricated line spans). It tells you who to ask.
The session is the answer — what the agent was asked, what it reasoned,
what it tried and reverted, where it went beyond its brief.

dkod keeps both, linked: the trace record for interop, the full transcript
for forensics (`dkod blame` resolving a line to its prompt, `dkod drift`
flagging where output exceeded intent). Export the pointer freely; the
answer stays in your repo.

## Interop over empire

The honest strategic note: attribution formats have network effects, and a
single vendor defining the de-facto standard unilaterally would be bad for
everyone downstream — including us. Supporting Agent Trace keeps the format
layer open and lets dkod compete where we actually differentiate: capture
breadth, in-repo permanence, line-level blame, and drift.

If you maintain a tool that consumes Agent Trace and dkod's export loses
or misstates anything, that's a bug — issues welcome:
https://github.com/dkod-io/dkod-cli

Install: `brew install dkod-io/tap/dkod`
```

## Notes for the founder

- Replace `v0.X.X` with the actual release that ships the export. Command
  shape verified against the implementation: `dkod export agent-trace
  [<session-id>] [--out <dir>|-]` (see `docs/agent-trace.md` for the field
  mapping and the spec pin: v0.1.0, repo commit `2754f077`).
- Re-verify the Agent Trace participant list and spec activity at publish
  time (spec has been dormant since Feb 2026 — if that's still true, the
  "keeps the format layer open" framing gets stronger, but don't claim the
  spec is "thriving").
- Cross-link the refs/dkod format spec doc (launch blocker item) — the two
  documents reinforce each other's "open format" story.
