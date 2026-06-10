# Launch + outreach artifact pack

Ready-to-send drafts for the dkod v0.2.0 launch, all consistent with
[`docs/plans/2026-06-10-dkod-strategy-v2.md`](../plans/2026-06-10-dkod-strategy-v2.md).
Every outbound artifact goes out under the founder's own name — nothing here
is auto-sent.

Hard facts every artifact adheres to: v0.2.0 is live; install is
`brew install dkod-io/tap/dkod` or
`curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh`;
7 agents captured; tagline **"Transcripts are never stored outside your git
host."** Banned claims (never use, anywhere): "Entire only supports 2
agents" (false — they support 10), "clones see sessions automatically"
(false — teammates run `dkod init`), "regulation requires AI-code
provenance" (false until at least Dec 2027 — say "audit-ready evidence").

## The artifacts

| File | What it is | Needs human to send |
|---|---|---|
| [`show-hn.md`](show-hn.md) | Show HN title + body + maker first-comment + posting mechanics | **YES** — founder posts on HN, replies live for hours |
| [`pilot-tldraw.md`](pilot-tldraw.md) | GitHub comment for tldraw's closed-PR policy thread: "PRs reopen if a verifiable session is attached" | **YES** — founder's GitHub account; viewer link must be live first |
| [`pilot-ghostty.md`](pilot-ghostty.md) | Short email/DM to Mitchell Hashimoto: opt-in "session attached ⇒ priority review" lane | **YES** — founder's email; viewer link must be live first |
| [`reddit-series.md`](reddit-series.md) | 3 r/ClaudeCode posts (blame, transcript rescue, drift) + r/ClaudeAI adaptation, with cadence/authenticity rules | **YES** — founder's aged Reddit account, 1/week, live engagement |
| [`discovery-calls.md`](discovery-calls.md) | 20-call demand-authenticity script, 8 questions, <5/20 kill rule, 20 target types | **YES** — founder-led calls; gates ALL compliance messaging |
| [`team-tier-gate.md`](team-tier-gate.md) | Design note: free ≤5 seats/1 org, 6th-seat/2nd-org hard gate, limit UX, hybrid-pricing hedge | No — internal build spec for the indexer |
| [`agent-trace-post.md`](agent-trace-post.md) | Blog post "dkod speaks Agent Trace" — interop positioning | **YES** — publish only when export/import ships |
| [`moonwell-case-study.md`](moonwell-case-study.md) | Blog post: $1.78M exploit traced via trailers; trailers say WHICH model, sessions say WHAT/WHERE | **YES** — publish + influencer pitches; re-verify sources first |

## Launch-sequence checklist (strategy order)

### Prerequisites (launch blockers — before anything below goes out)

- [ ] v0.2.0 tagged + brew tap + curl install.sh verified end-to-end (DONE: v0.2.0 is live — re-verify install one-liners anyway)
- [ ] Redaction hardening shipped (entropy rules + audit note + docs page)
- [ ] Tagline fixed everywhere ("never **stored** outside your git host")
- [ ] 5 pre-pivot repos archived with README redirects
- [ ] Zero-install web session viewer live (hard dependency of both pilots)
- [ ] Drift card screenshot + 20-second blame GIF produced

### Week 1 — pilot outreach + groundwork

- [ ] Send `pilot-tldraw.md` (GitHub comment) — **human sends**
- [ ] Send `pilot-ghostty.md` (email) — **human sends**
- [ ] Begin `discovery-calls.md` sourcing (first calls booked) — **human runs**
- [ ] Claude Code plugin + awesome-list + MCP directory submissions
- [ ] Publish refs/dkod format spec as an open doc; publish real pricing page

### Week 2 — staging

- [ ] Discovery calls running (target 3–5 done)
- [ ] Drift precision sanity pass (tune to under-flag before the card goes viral)
- [ ] `team-tier-gate.md` reviewed → indexer implementation scheduled (internal)
- [ ] Pilot follow-ups if either target responded

### Weeks 3–4 — launch

- [ ] Post `show-hn.md` (Tue–Thu 9–12 ET or Mon 00:00 UTC; maker comment in 5 min; stay online 4h) — **human sends**
- [ ] If Agent Trace export shipped: publish `agent-trace-post.md` same week — **human sends**; else hold
- [ ] Start `reddit-series.md` Post 1 (blame) on r/ClaudeCode — **human sends**
- [ ] If HN whiffs: one repost + second-chance email to hn@ycombinator.com

### Weeks 5–6 (month 2 begins) — amplify

- [ ] Reddit Posts 2 (transcript rescue) and 3 (drift), one per week — **human sends**
- [ ] Publish `moonwell-case-study.md` — **human sends**
- [ ] Influencer pitches off the Moonwell story (IndyDevDan, Simon Willison, steipete, swyx; Theo/Primeagen get the slop STORY) — **human sends**
- [ ] r/ClaudeAI adaptation of the transcript-rescue post (1–2 weeks after the r/ClaudeCode version) — **human sends**
- [ ] Discovery calls complete (20/20) → apply the <5/20 kill rule to all compliance messaging

### Ongoing gates

- Trust kill-criterion: any un-redacted secret pushed → freeze ALL launch
  marketing until fixed, public post-mortem.
- Distribution check at day 60: <1,000 installs or <500 stars despite all
  channels → rethink entry product (lead with transcript-rescue hook).
