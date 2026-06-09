# dkod vs. Macroscope — Competitive Positioning & Roadmap

**Date:** 2026-05-27
**Status:** approved (positioning + near-term roadmap), ready for implementation planning per feature

## TL;DR

Macroscope is not the same product as dkod, but it competes for the same
narrative ("visibility into AI-assisted engineering") and the same buyer
(eng leaders), and it is well-funded. We do **not** reposition away. We
sharpen: dkod leads with **provenance & forensics for AI-written code**
(the agent *session* is the artifact), with **git-native / privacy** as
the architectural reason that artifact can safely exist. Macroscope
analyzes *outputs* (commits, PRs, tickets) server-side; dkod records the
*process* (prompt → reasoning → tool calls → diff → commit) and keeps it
in the customer's git host. They cannot reach our slice without
abandoning their architecture.

The defensible asset is not any single feature — it is a **compounding,
proprietary corpus of outcome-linked agent sessions**. Every roadmap
item below is a withdrawal from that asset, and the asset grows more
valuable with every session captured.

## The competitor, factually

(Verified 2026-05-27 against macroscope.com/status, TechCrunch
2025-09-17, and search.)

- **What it is:** primarily an **AI code-review** platform — AST-based
  bug detection (claims 5% more bugs, 75% fewer comments than rivals),
  auto PR descriptions, auto-fix, custom markdown check-runs.
- **"Status" / visibility:** consumes commits, PRs, Jira/Linear tickets,
  and git log → commit summaries, sprint reports, weekly Slack digests,
  and "productivity stats for devs **and agents**." Audience: leaders /
  managers.
- **"Agent":** a RAG Q&A bot grounded in codebase + git log + PRs /
  tickets, available via Slack / API / GitHub.
- **Company:** founded July 2023 (Kayvon Beykpour, ex-Periscope→Twitter,
  + Joe Bernstein + Rob Bishop). **$40M raised** ($30M Series A led by
  Lightspeed; Thrive Capital, Google Ventures, Adverb). ~20 employees,
  SF. Pricing ~$30/active dev/mo (5-seat min) plus a usage-based tier.

### Where we actually overlap

Only at the manager-dashboard layer. Macroscope markets "agent
productivity stats," but those are **inferred from git/PR metadata** —
they can say *"this agent produced these commits/PRs."* They **never
ingest the session**, so they cannot show the prompt, the reasoning, or
the tool calls, and cannot search across that process. They analyze what
changed; we record what the agent did and why.

### Reframe: Macroscope is partly validation

A $40M Lightspeed/Thrive/GV bet that "leaders need visibility into
AI-assisted engineering" proves the buyer and the budget exist. dkod
rides that category-creation and occupies the slice their server-side,
output-only architecture cannot serve.

## Positioning: A + B

### A — Provenance & forensics for AI-written code (headline)

**Job to be done:** *"When AI-written code breaks, or does something I
don't understand, show me exactly what the agent was told and what it
did — searchable across the org."*

dkod is not "is this PR good?" (Macroscope). dkod is "what did the AI
actually do, and can I trace it?" The session — prompt + reasoning +
tool calls — is the primary artifact, tied to the resulting diff and
commit.

### B — Git-native / privacy (the enabler, not the headline)

Transcripts never leave the customer's git host; git is the source of
truth; the indexer is a disposable cache. This is the architectural
reason the provenance data in (A) can exist at all without becoming a
data-liability SaaS. It is also a direct counter to Macroscope's
server-side ingestion: it wins the security-sensitive orgs
(fintech / healthcare / defense / regulated enterprise) they
structurally cannot serve.

**Why A leads and B supports:** "visibility" is the contested word
Macroscope already owns. We avoid competing on it directly by anchoring
to a concrete job (forensics / provenance / accountability). Privacy is
the trust substrate that makes the job credible — only a git-native
design can hold full sessions without becoming a liability.

### The honest risk

This is a **timing bet**. The open question is whether *"I need to trace
what my agents did"* is an acute 2026 pain or a 2027 pain. That — not
Macroscope's feature set — is the real strategic risk. Mitigations:
`dkod blame` (below) makes the value visceral with a single demo even
before forensics is a budgeted line item; and the free CLI keeps
adoption cost near zero while the pain matures.

## The moat: a compounding corpus

dkod's durable advantage is a dataset no competitor has: real,
cross-agent, cross-repo, org-wide agent sessions, each linking
**intent → action → outcome**. Macroscope sees outputs; vendor
dashboards (Anthropic / OpenAI) see one agent in isolation; code-review
tools see diffs. Only dkod holds the full triple, federated across the
org. The corpus has network effects — more sessions make every feature
below better — which a point-tool cannot replicate.

## Roadmap beyond the team dashboard

Selected net-new features, ordered by role. Each requires the corpus and
is offered by no competitor today.

### 1. `dkod blame` — git-blame for AI *(SHIPPED — V1)*

Point at any file:line → the exact session and prompt that produced it.
Per-line attribution back to agent intent. This is the visceral demo
that makes provenance (A) tangible in five seconds, and it extends a
mental model every developer already has (`git blame`).

**Status: implemented** (`docs/plans/2026-05-27-dkod-blame.md`). V1 ships
two pieces: (a) HEAD-watching session→commit linkage — the capture flow
records repo HEAD at session start and links every commit reachable from
HEAD at session end to the session via `refs/dkod/commits/<sha>`; and
(b) the `dkod blame <path>` command, which shells out to
`git blame --porcelain` (gix 0.66 has no blame) and maps each line's
commit through that ref to its session, rendering agent + short session
id + prompt summary; human commits show normally and working-tree edits
render as `(uncommitted)`.

Commit-linking now spans **all agents**: the Claude Code server path plus
the six synchronous wrappers (codex, copilot-cli, cursor, factory-ai,
gemini-cli, opencode) each records HEAD before the agent runs and links the
commits it produces via a shared `finalize_session` helper. So `dkod blame`
attributes lines regardless of which agent wrote them.

History-rewrite re-linking now ships
(`docs/plans/2026-06-03-history-rewrite-relinking-design.md`): a per-repo
`post-rewrite` git hook (installed by `dkod init`) pipes git's old→new SHA
pairs to `dkod relink`, which re-points `refs/dkod/commits/<new>` at the
session blob — so rebase / `commit --amend` / squash / reword keep their
provenance and `dkod blame` resolves the rewritten lines.

A **patch-id fallback** now covers the rewrites the hook never sees
(`docs/plans/2026-06-09-patchid-fallback-design.md`): at capture, each
produced commit also gets a `refs/dkod/patchid/<patch-id>` → session ref;
at blame time, a commit-ref miss falls back to matching the line's commit
`git patch-id`, which is stable across **diff-preserving** rewrites. So
history rewritten **before `dkod init`**, rewrites on a **clone without the
hook**, and `git filter-repo` now recover provenance too (these refs travel
with the repo via the existing `+refs/dkod/*` refspec). The hook and the
patch-id fallback are complementary: the hook owns squash and live local
rewrites; patch-id owns diff-preserving rewrites that bypass it.

Remaining residue (documented limitations): **squash** is lossy
(last-writer-wins — the squashed commit attributes to one session) and is
not recoverable by patch-id (its diff changes); **content-changing**
rewrites are genuinely different work (no attribution, by design); patch-id
**collisions** (identical diffs) are last-writer-wins; sessions captured
**before** this shipped have no patch-id ref (a future `dkod reindex` could
backfill); `core.hooksPath` users wire the hook manually; and `dkod show`
may list a pre-rewrite SHA (re-linking updates refs, not the session blob's
commit list).

### 2. Org session memory *(the company-defining bet)*

*"Has anyone's agent solved this before?"* Semantic search / RAG over
the org's actual agent problem-solving history → surface the prompt and
approach that worked. This converts the session corpus into reusable
institutional knowledge and is the feature with genuine **network
effects**: its value compounds with every captured session, and it
cannot be cloned without the corpus. This is the long-term moat and the
strongest reason dkod is a company and not a feature.

### 3. Intent-vs-output drift *(safety / oversight primitive)*

Compare what the prompt asked for against what the agent actually changed
→ flag sessions where the agent did materially more / other than asked
("told to fix a typo, also rewrote auth"). Only possible because dkod
holds prompt and diff together. Feeds the oversight narrative and pairs
naturally with the privacy/compliance angle of (B).

### Parked (revisit later)

- **Prompt-surface leak forensics** — scan the corpus for secrets / PII
  pasted into prompts or pulled into agent context. A net-new security
  surface nobody monitors; strong reinforcement for (B), deferred to keep
  near-term scope focused.
- **Regression → session bisect**, **session replay/fork**, **grounded
  agent/prompt eval** — good follow-ons once the corpus and forensics
  surfaces exist.

## What this means for execution

- **Don't** chase code-review parity with Macroscope. That is their core;
  it is not our game.
- **Do** ship the provenance surfaces (`dkod blame` first) that only the
  session corpus enables.
- **Do** treat every product decision as "does this make the corpus
  bigger / more valuable / more trusted?" — that is the compounding asset.
- **Keep** the free MIT CLI + offline desktop app as the zero-friction
  adoption wedge; the corpus and the team layer are the business.

## Open questions for implementation planning

1. `dkod blame` needs per-line → session mapping. Does the captured
   session already carry enough diff/line metadata to support this, or is
   a capture-format change required?
2. Org session memory implies embeddings + semantic search in the
   indexer. Where does that compute live, and how does it honor the
   trust boundary (content fetched on demand under the user's token, not
   persisted)?
3. Intent-vs-output drift needs a reliable "what was asked" signal. Is
   the prompt alone sufficient, or do we need to capture an explicit
   task/intent marker?
