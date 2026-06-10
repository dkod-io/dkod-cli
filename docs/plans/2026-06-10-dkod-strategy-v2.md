# dkod Strategy v2 — Direction, Monetization, Growth

**Date:** 2026-06-10
**Status:** approved direction, supersedes the positioning sections of
`2026-05-27-macroscope-competitive-positioning.md` where they conflict
**Research basis:** 31 research/verification agents — 9 market-research topics
(each adversarially fact-checked), an internal asset audit, 3 independent
strategy panels, 3 judge panels, a completeness critic, and 6 gap-closing
follow-ups (ICP sizing, GitLab/Atlassian, Entire teardown, custom-refs live
verification, conversion benchmarks, OSS-policy channels). Load-bearing
internal claims re-verified by hand (indexer live at dkod-indexer.fly.dev,
redaction shipped in `redact.rs`, `feat/multi-git-host` now backed up to
origin).

---

## The decision

**Keep the direction. Fix the execution.** dkod stays the **git-native flight
recorder for AI coding agents** — capture every agent session into the
customer's own git repo, answer "which prompt wrote this line, and did the
agent exceed its brief" (`dkod blame` / `dkod drift`) — monetized
Tailscale-style: free data plane (the CLI + the customer's git host), paid
thin control plane (org-wide search, drift digests, ACLs) that never stores
transcripts.

Three judge panels scored this unanimously above a compliance-first
repositioning and a memory-first pivot. The compliance wedge is the right
**2027 second act** (EU AI Act high-risk duties slipped to Dec 2027/Aug 2028;
coding assistants are mostly out of scope anyway); the memory wedge sits
directly on Anthropic/Cursor/GitHub native roadmaps and dies when they bundle
it. Forensics is the one lane where every funded competitor is **structurally
prevented** from following: Entire's own architecture doc rejects per-line
blame ("would require reimplementing git-blame" — which is exactly what we
built) and labels its attribution "informational, not security-critical";
Macroscope ingests zero session data; GitHub /chronicle is Copilot-family
only; Git AI keeps prompts out of the repo; eng-intelligence suites
(Jellyfish/DX/LinearB) are metadata-only.

**The binding constraint is distribution, not product.** 0 stars, 1 download,
and every flagship feature sits unreleased past the last tag. The plan below
converts a distribution problem — the cheapest problem a built product can
have — into a launch sequence.

One honest reframe of expectations: **year 1 is install-base and corpus
accumulation, not revenue.** Recalibrated against real benchmarks (ChartMogul
2026: freemium GOOD 3–5%, GREAT 8–12%; Slack's 30% needs a hard collaboration
gate; Tailscale took ~5 years to $45M ARR), base case is ~$340 MRR at month 6
and ~$2k MRR at month 12. The original "15% of activated orgs convert" was
top-decile. Real monetization window: months 18–36. The architecture makes
this survivable: infra is ~$30–50/month at zero customers, >90% margin at
every scale.

## What dkod solves, and for whom

**Job-to-be-done:** *"When AI-written code breaks, gets reviewed, or gets
audited — show me exactly what the agent was asked, what it actually did, and
where it drifted; across every agent my devs use, without adding a new data
processor to my vendor register."*

Three moments where this is worth money:

1. **Review time** — 66% of devs fight "almost right" AI code (SO 2025);
   <50% review AI code before committing (Sonar). The drift digest is a
   review-burden killer, which is how the market actually articulates this
   pain (never "session archival" — SpecStory positioned there and got ~3k
   actives in 18 months).
2. **Incident time** — replay the session that wrote the line that broke
   prod. The Moonwell exploit ($1.78M, Feb 2026) was traced via nothing more
   than `Co-authored-by: Claude` trailers; trailers say *which model*, dkod
   says *what it was asked, what it reasoned, where it drifted*.
3. **Audit time** — SOC 2 AI acceptable-use evidence, ISO 42001
   proof-of-execution, US Copyright Office AI-disclosure, M&A diligence.
   Sell as "audit-ready evidence", never "regulation requires this" (it
   doesn't, yet).

**ICP (reframed by follow-up research):** senior engineers / tech leads on
10–100-dev teams running 2+ agents, in orgs that refuse **persistent
third-party storage of code** while permitting contracted inference
(Bedrock/Vertex in their own account, ZDR addenda). That intersection is
thousands to low-tens-of-thousands of orgs. Priority cells:

- **GitLab Self-Managed** (~70% of GitLab's >$1B ARR; GitLab now ships Claude
  Code/Codex as managed external agents to them — and auto-deletes its own
  session records after 30 days: *"GitLab keeps your agent sessions for 30
  days; dkod keeps them forever, in your repo, blameable per line"*).
- **GitHub Enterprise Server** (Copilot-restricted → BYO-agent is forced).
- **Anthropic ZDR / Bedrock / Vertex orgs** — they lose vendor analytics
  entirely under ZDR; they want provenance and refuse retention. *"Works with
  your Bedrock deployment; transcripts never stored outside your git host"*
  is the sharpest sentence for this buyer. (Verify + document Bedrock/Vertex
  capture as a build item.)
- **DeFi / security-audited codebases** — provenance buying intent already
  exists (Moonwell).
- **Dropped:** Bitbucket Data Center as a named segment (<1% of Atlassian
  customers, EOL March 2029, and its 4GB hard repo cap is hostile to
  transcript storage).

**The buyer** is the VP Eng / Head of Platform already paying $20–59/dev/mo
for metadata-only eng-intelligence — and increasingly the **AI Governance
Board** that approves agent rollouts (Black Duck, Mar 2026: 68% of teams call
automated tracking of AI-generated code "extremely important"; only 30% are
fully governed). Positioning line for them: *"the audit trail that gets your
agent rollout past the governance board."*

## Positioning vs the field (June 2026, all verified)

| Player | What they are | dkod's true differentiation |
|---|---|---|
| **Entire** ($60M seed, Dohmke, 10 agents, in-repo `entire/checkpoints/v1` branch + hosted viewer) | Closest architecture. No pricing, zero named customers, star growth decayed to ~200/mo | **Per-line, audit-grade blame** (they rejected it by design; theirs is "informational, not security-critical"); **drift** (unshipped there, but "Intent-Based Review" is on their roadmap — clock is ticking); cleaner git citizenship (hidden refs vs a branch in every `git branch -a`); capture-time redaction default-ON vs their bolted-on post-leak redaction. **Never say "they only support 2 agents" — they support 10, a superset of our 7.** |
| **Git AI** (2k stars, owns "git blame for AI" phrase) | Line attribution in git notes; prompts kept OUT of repo (local SQLite or their cloud) | Full-transcript provenance IN the repo: blame without the session can't do drift or intent audit; their team tier pulls prompts into their cloud |
| **SpecStory** | Session archival to markdown; capture+cloud free | Proof capture-as-product doesn't sell. We sell forensic answers, not transcripts |
| **Macroscope** ($40M, usage-priced, GitHub-only SaaS) | Reviews agent OUTPUT; ingests zero session data | Complementary, not head-on: "Macroscope tells you the PR is correct; dkod tells you which prompt produced line 142." Their server-side, GitHub-only model cedes our ICP entirely |
| **GitHub /chronicle + session sync** (June 2026) | Copilot-family only, server-side, 24h enterprise search | Agent-neutral, any-host, in-repo, line-level. Hedge segment = non-GitHub + ZDR. Realistic window: 6–12 months |
| **Atlassian DX "AI Code Insights"** (GA Apr 2026) | **Most important competitor to watch.** Hooks the same capture points (Claude Code/Copilot/Cursor), full session capture, line-level tracking — into DX Data Cloud SaaS | Their data flows to Atlassian's cloud; ours stays in the customer's git host. Theirs feeds ROI dashboards; ours is per-line forensics. Kill-trigger: DX line attribution appearing in Bitbucket's blame/PR UI |
| **GitLab Duo Agent Platform** | Stores agent sessions in the GitLab DB — auto-deleted after 30 days, no line attribution | Permanence + portability + line-level. Kill-trigger: GitLab ships persistent/exportable session retention or line-level attribution |
| **Eng-intelligence** (Jellyfish $114.5M, DX→Atlassian $1B, LinearB) | Metadata-only AI measurement | We are the ground-truth transcript layer under their dashboards, not another dashboard. Partnership/acquisition optionality: they need attribution signal we have |
| **h5i** (Apache-2.0 Rust, refs/h5i/*, 318 stars) + **agentblame** | Architectural twin / blame-niche OSS | Forensics depth + 7-agent breadth + the rewrite-surviving link chain (post-rewrite hook + patch-id fallback). Watch both |
| **Agent Trace** (cursor/agent-trace spec; Cursor, Cognition, Cloudflare, Vercel, git-ai) | Storage-agnostic attribution format, dormant since Feb | Don't compete — **adopt**. "dkod is the git-native Agent Trace store." Interop is a free distribution wedge and blocks Entire defining the format unilaterally |

**Tagline fix (launch blocker):** "transcripts never leave your git host" is
falsified by the hosted indexer persisting transcript-derived summaries +
embeddings — the first hostile HN comment will say so. New line:
**"Transcripts are never stored outside your git host."** Derived
summaries/embeddings become per-org opt-in with explicit framing (and
client-side embedding lands on the roadmap). The fuller enterprise line:
*"zero third-party data-at-rest; inference stays in the model boundary you
already approved; zero new processors in your vendor/DORA register."*

## Product plan (half-scope, per the skeptic judge: budgets assume ~20 h/wk)

### Keep (already built, undermarketed)
- MIT Rust CLI: capture (7 agents), `blame` (+ post-rewrite relinking +
  patch-id fallback), `drift`, log/show, setup wizard, redaction (5 builtin
  rulesets, default ON).
- The deployed Fly.io indexer with the implemented trust boundary
  (metadata-only persistence, user-token content fetch, disposable index).
- dkod.io site, refs/dkod/* format.

### Launch blockers (weeks 1–2)
1. **Tag v0.2.0 + brew tap + curl install.sh as headline installs.** Every
   demo-able feature is unreleased; viral CLIs are one-command installs.
2. **Redaction hardening** — not from scratch (it ships, default ON):
   add entropy-based generic-credential detection, an audit note of what was
   redacted, and a documented redaction page. Entire leaked gitignored
   secrets for ~2 months; their docs now concede "best-effort." Ours becomes
   a headline: *capture-time redaction, on by default, since V1.* Trust
   kill-criterion: any un-redacted secret pushed = freeze launch marketing
   until fixed.
3. **Tagline fix** across site/README/docs (above).
4. **Archive the 5 pre-pivot repos** (dkod-swarm, harness, dkod-plugin,
   dkod-engine, pi-extension) with README redirects — they outrank the
   product and market a contradictory pitch under the same brand.
5. Update stale internal docs (CLAUDE.md says the indexer is "planned" — it
   is deployed and healthy).

### Growth wiring (weeks 2–4)
6. **The honest in-repo viral loop** (verified: clones do NOT see refs):
   - `dkod init` runs `git ls-remote origin 'refs/dkod/*'`, detects existing
     sessions, announces *"This repo has 1,234 captured sessions from 12
     contributors — fetching index…"*
   - A committed breadcrumb (`.dkod.toml` + optional README badge "🗂 N agent
     sessions captured — `dkod init` to browse") so teammates learn dkod
     exists at all.
   - Optional session links in PR descriptions — the artifact reviewers see
     pre-install.
7. **Screenshot-engineered drift card** + a 20-second `dkod blame` GIF
   (ccusage built 15.9k stars on screenshot-shareable terminal output; every
   "my agent went rogue" X thread is free distribution).
8. **Claude Code plugin** + submissions: anthropics/claude-plugins-official,
   hesreallyhim/awesome-claude-code (~37–46k★), MCP directories
   (PulseMCP/Smithery).

### Storage v2 (months 2–3) — **required before any 50-dev org lands**
Live verification found the scaling bomb: median session ≈ **600KB
compressed** (not 50KB), 2–3 refs/session → a 50-dev org produces tens of
GB/year and ~250–375k refs/year; breaches Bitbucket's 4GB hard cap in weeks
and GitLab Free's 10GiB in 1–2 months; and `dkod init` currently subscribes
every remote's fetch refspec, so every teammate's every fetch pays the full
archive cost (~30MB of ref advertisement per fetch at 300k refs).
- **Rollup index**: one `refs/dkod/index` ref → commit → tree mapping
  session-id → blob (Gerrit NoteDb pattern). Kills O(sessions) ref count,
  enables real incremental fetch via commit negotiation. Commit-link and
  patch-id lookups become tree paths.
- **Size budget**: cap per-message tool output; ~256KB compressed per-session
  default budget with full-fidelity opt-in (plausible 5–10x cut — bloat is
  tool-output spam).
- **`dkod gc --keep <duration>`** + bundle/indexer archival before deletion.
- **Lazy propagation default**: on-demand fetch (`dkod show`/`blame` fetch
  just-in-time); `dkod init --subscribe` for full mirror.
- Side-repo (`<repo>-dkod`) as documented escape hatch for quota-capped
  hosts, plus the **sessions-only separate remote** as a first-class
  deployment mode for security teams that ban transcripts in work repos
  (the #1 enterprise objection no redaction fully solves).

### Interop + evidence (months 2–4)
9. **Agent Trace export/import** — "dkod speaks Agent Trace" (itself a launch
   artifact).
10. **Importers**: `~/.claude/projects` (rescue transcripts from 30-day
    expiry — *"dkod saves your sessions before they're deleted"* is the
    personal hook), `~/.codex/sessions`, Copilot agent-tasks API. Platforms'
    own session stores become dkod feedstock.
11. **`dkod report` evidence pack** (local, zero-network; Sigstore-signed
    export): AI-authored % per file, session links, drift summary — MD/PDF
    for SOC 2 / ISO 42001 / Copyright Office / diligence.
    **Demand-authenticity gate:** 20 structured discovery calls first; <5
    confirming a real auditor/insurer/acquirer ask in the past 12 months →
    drop compliance messaging entirely. (The M&A diligence SKU is mostly
    fictional in year 1 — it only works on repos that already ran dkod.)

### Paid team layer (months 3–5, GA month 5 — not month 3)
12. On the **existing** indexer: org-wide cross-repo session search, weekly
    drift digest, role-based session-read ACLs, capture-coverage monitoring
    (% of merged PRs with a linked session — the honest answer to silent
    adapter breakage, and a metric leads check weekly), Stripe billing,
    GitHub+GitLab(+Gitea) via the multi-git-host branch.
13. **Self-hosted, license-key-gated indexer container** (enforced keys,
    never honor-system — Obsidian measured ~90% non-compliance) as the
    Business tier for air-gapped/regulated buyers. Zero infra for us.
    (CodeRabbit's self-hosted tier at ~$15k/mo for 500 seats is the price
    anchor proving this segment pays.)
14. **MCP recall server** (months 4–6, retention feature, never the wedge):
    `recall_sessions` / `how_was_this_solved` with retrieval-with-citation.
    Converts an insurance-shaped product into a daily habit; A/B "agents
    with memory" vs "blame and drift" framing once it exists.

### Drop / never
- Tauri desktop viewer (web viewer on the indexer covers it; revisit on pull).
  **Exception:** the OSS-maintainer channel needs a zero-install web session
  viewer (shareable link rendering a session read-only) — that's a thin
  indexer page, not a desktop app.
- Productivity dashboards (LoC/accept-rate/leaderboards) — platforms bundle
  them free; bossware backlash kills deals. Explicit no-individual-
  leaderboard stance, ever.
- Compliance-mandate marketing (false until at least Dec 2027).
- CLI monetization in any form; honor-system licensing.
- Memory as the company bet.
- The "2 agents vs 7" talking point (false), the "clone and see sessions"
  claim (false), the unqualified "transcripts never leave your git host"
  tagline (falsified by our own indexer).

## Monetization

| Tier | Price | Contents |
|---|---|---|
| **Free forever** | $0 | MIT CLI (capture/blame/drift, all agents) + hosted indexer ≤5 seats / 1 org |
| **Team** | $15/seat/mo ($12 annual) | Unlimited repos, org-wide session search, weekly drift digest, session-read ACLs, capture-coverage monitor, multi-host |
| **Business** | $25/seat/mo | SSO/SCIM, audit log, retention policies, scheduled evidence packs, **self-hosted indexer (license key)** |
| Metered | cost +10% | Any LLM-powered feature (semantic search, drift explanations). Never subsidize inference (the Zed lesson) |

- $15 sits inside the proven 2026 corridor (Raycast Teams $12, Postman $19,
  Copilot Business $19, CodeRabbit $24, Sentry $26, Greptile $30) and under
  Macroscope's old $30 anchor.
- **The conversion lever is the gate, not the price.** Conversions cluster
  within 7 days of hitting a hard limit vs 90–180 days for value-upsell.
  The free→Team line is a hard collaboration gate (seats + shared search),
  Slack/Sentry/Postman-style — not a dashboard upsell.
- Hedge against seat-erosion in agent-heavy orgs (Macroscope's pivot
  rationale): be ready with an org-flat + per-seat hybrid.
- Diligence reports: opportunistic only, founder-delivered, $5k flat — not in
  the revenue plan.

**Recalibrated revenue path** (benchmarks: freemium GOOD 3–5% / GREAT 8–12%
org-level; AI-native GREAT 15–20% only with hard gating; ~10% of installs
clustering into ≥3-dev orgs):

| | Month 6 | Month 12 |
|---|---|---|
| Conservative | ~$0–75 MRR | ~$150 MRR |
| **Base** | **~$340 MRR** | **~$2,000 MRR (~$24k ARR)** |
| Optimistic (hard gate + 15k installs) | ~$3,200 MRR | ~$21,600 MRR (~$260k ARR) |

Year 1 = corpus + install accumulation. Months 18–36 = the monetization
window, with the compounding corpus and capture-coverage habit as the
retention engine. This is the Tailscale/Sentry sequence, and the
near-zero-infra architecture is what makes it survivable solo.

## Go-to-market (first 90 days)

**Channel #0 — the OSS pilot (highest leverage, start outreach week 1):**
The AI-contribution-policy wave converged on self-reported `Assisted-by:`
trailers (Linux kernel, Fedora, LLVM, ASF) that **no project can verify** —
dkod's session refs are the missing verification layer. Best pilot targets,
by their own stated positions:
1. **tldraw** (auto-closes ALL external PRs, explicitly "temporary pending
   better platform tooling" — we are that tooling: "PRs reopen if a
   verifiable session is attached").
2. **Ghostty / Mitchell Hashimoto** ("if you're going to use AI, you better
   be good" — the session is the driver's flight recorder).
3. **Fedora** (policy text anticipates iteration; already uses
   `Assisted-by:`).
4. **MicroPython / Godot** (mandatory per-PR disclosure, "frequently
   ignored").
5. **curl / Python security intake** ("attach the session that produced this
   finding" costs honest reporters nothing and slop-farmers everything).
Pitch as **carrot, never gate**: "session attached ⇒ priority review."
Dependency: the zero-install web session viewer. One named pilot converts the
launch from a tool announcement into *the* story (HN/Changelog/Register all
cover AI-slop weekly; curl killed its $86k bounty over it; Jazzband shut
down).

**Weeks 1–2 (unblock):** v0.2.0 + brew/curl; redaction hardening; tagline
fix; archive old repos; publish pricing page with real numbers; plugin +
awesome-list + MCP submissions; publish the refs/dkod format spec as an open
doc; begin pilot outreach (tldraw + Ghostty first).

**Weeks 3–4 (launch):** Show HN — *"Show HN: dkod — git-native blame and
drift for AI coding agents (transcripts never stored outside your git
host)"* with the blame GIF + drift card. Tue–Thu 9–12 ET (or Mon 00:00 UTC
per the 188k-post dataset), maker comment in 5 min; one repost / second-
chance-pool email if it whiffs. Same week: "dkod speaks Agent Trace" post.
r/ClaudeCode + r/ClaudeAI series begins (292k + 911k members; repeatable).

**Month 2 (amplify):** Moonwell case study (*"trailers told auditors WHICH
model; dkod tells you WHAT it was asked and WHERE it drifted"*); influencer
pitches — IndyDevDan (best topical fit), Simon Willison (highest-credibility
nod; one post took superpowers 0→2,000 stars/day), steipete, swyx/Latent
Space; pitch Theo/ThePrimeagen the *AI-slop story*, not the tool. Drift-card
reply loop on agent-gone-rogue X threads.

**Month 3 (monetize):** Team beta with 10 design partners filtered for
GitLab-self-managed / ZDR / Bedrock orgs; "evidence pack for your AI
Governance Board" content; GitHub Marketplace listing (95% rev share,
billing channel only). Paid GA month 5.

**Plan B if HN whiffs twice** (ranked by measured yield): r/ClaudeCode series
(200–1,500 installs over weeks), one influencer video (500–5,000), Changelog
News + interview (250–1,100), free TLDR editorial mention (100–500), Product
Hunt (100–400), conference talks for pilot-partner pipeline. Composite
realistic month-1: **1,500–6,000 installs**.

## Infra (unchanged thesis, now with guardrails)

- Data plane: customer's git host + laptops. Zero marginal cost by
  construction.
- Control plane: existing Fly.io app (scale-to-zero, ~$5–30/mo idle) +
  Postgres/pgvector (Neon free tier → $19/mo), reconciler-only ingest via
  `git ls-remote` (no webhook fleet), OAuth per host, Stripe. Metadata +
  opt-in summaries/embeddings only; transcripts fetched under the user's
  token; the whole index is a disposable cache — no backup/DR/breach surface.
- Cost: ~$30–50/mo at zero customers; ~$1–2/org/mo marginal; >90% gross
  margin at every scale; no on-call, no GPU fleet, no data pipeline.
- **Infra kill-criterion:** hosted cost >20% of MRR → stop adding hosted
  features.
- Self-hosted Business container = zero infra for us at the high end.

## Risks & kill criteria

| Risk | Watch / trigger |
|---|---|
| **Entire ships "Intent-Based Review"** (on their roadmap) | Drift moat has a clock — ship + publicize drift now; re-differentiate on audit-grade per-line + any-host if they land it |
| **GitHub extends /chronicle beyond Copilot family** | 6–12 mo realistic window. Trigger: cross-agent ingestion + org search → re-evaluate hosted thesis in 30 days; fall back to non-GitHub + evidence wedge |
| **GitLab ships persistent sessions / line attribution** | Kill-trigger added (today: 30-day auto-delete, no line attribution) |
| **Atlassian wires DX line-attribution into Bitbucket UI / Rovo sessions into repos** | Kill-trigger added; test coexistence with the DX daemon + GitLab Duo CLI on shared machines |
| **Drift precision unproven** (headline feature, zero benchmark) | 30-day action: labeled benchmark from 50–100 public sessions (SpecStory histories etc.); measure precision; if the card is noisy the viral loop distributes embarrassment — tune to under-flag before launch |
| **Distribution failure** | <1,000 installs or <500 stars within 60 days of launch despite all channels → rethink entry product (likely lead with transcript-rescue/personal hook) |
| **Activation failure** | <25 free-tier orgs by month 4 → the Atuin failure mode; revisit the team gate |
| **Monetization failure** | <5 paying teams 90 days post-GA, or <$2k MRR by month 12, or org conversion <3% sustained → repackage once, then kill the hosted tier and ride the OSS/evidence wedge |
| **Trust incident** | Any un-redacted secret pushed → freeze launch marketing until fixed; post-mortem in public |
| **Corpus-moat falsification** | By month 9, zero design partners cite accumulated history as a reason to stay → drop the corpus thesis from valuation logic |
| **Storage scaling** | Any org >20 devs onboarding before Storage v2 ships → prioritize the rollup index immediately |

## 30-day action list (ordered)

1. ~~Push `feat/multi-git-host` to origin~~ **done 2026-06-10** (was the
   single-point-of-failure for the portability story).
2. Tag + release v0.2.0; ship brew tap + curl install.sh.
3. Redaction hardening (entropy rules + audit note + docs page).
4. Tagline + README + site copy fix ("never **stored** outside your git
   host"); publish real pricing.
5. Archive 5 pre-pivot repos; fix stale CLAUDE.md (indexer is live).
6. Clone-breadcrumb viral loop (`dkod init` ls-remote detection + committed
   marker).
7. Drift card output + blame GIF; drift precision benchmark.
8. Claude Code plugin + directory/awesome-list/MCP submissions.
9. Pilot outreach: tldraw, Ghostty (carrot framing); zero-install web session
   viewer spike on the indexer.
10. Show HN + Agent Trace post + r/ClaudeCode series.
11. Merge multi-git-host into the indexer; design the Team-tier hard gate.
12. Begin the 20 discovery calls (governance-board/audit demand
    authenticity).
