# Show HN draft

> **WHEN TO SEND / WHERE:** Hacker News (news.ycombinator.com/submit), weeks 3–4 of the launch sequence. Post Tue–Thu 9:00–12:00 ET, or Mon 00:00 UTC (per the 188k-post timing dataset). Founder posts under his own account. Maker comment goes up within 5 minutes of submitting. NEEDS HUMAN TO SEND.

## Title

```
Show HN: dkod – git-native blame and drift for AI coding agents
```

(URL: link the GitHub repo, not the landing page — Show HN convention favors the thing itself.)

## Body (submission text)

```
Hi HN — I built dkod because the evidence trail for AI-written code is
disappearing the moment it's created.

The problem: 66% of developers say their biggest frustration with AI code is
that it's "almost right" (Stack Overflow 2025), and fewer than half review
AI-generated code before committing it (Sonar). When that code later breaks —
or gets audited — the session that produced it is usually gone. When the
Moonwell DeFi exploit ($1.78M, Feb 2026) was investigated, the only provenance
anyone had was `Co-authored-by: Claude` commit trailers. Trailers tell you
WHICH model touched the code. They can't tell you what it was asked, what it
reasoned, or where it went beyond its brief.

What dkod does:

- Captures every AI agent session — Claude Code, Codex, Copilot CLI, Cursor,
  Factory droid, Gemini CLI, opencode (7 agents) — as git objects under
  `refs/dkod/sessions/<id>` in YOUR repo, on YOUR git host. No new service
  holds your transcripts.
- `dkod blame <file>` — like git blame, but it resolves a line to the prompt
  and session that wrote it. Survives rebases and rewrites via a post-rewrite
  hook with a patch-id fallback.
- `dkod drift` — compares what you asked the agent to do against what it
  actually changed, and flags where it exceeded its brief.

Privacy architecture, because this is transcripts of you talking to an agent
about your codebase: transcripts are never stored outside your git host.
Capture-time redaction is on by default (5 builtin rulesets) — secrets are
scrubbed before anything is written, not after a leak. The optional hosted
team layer indexes metadata only and fetches content under your own token;
the index is a disposable cache.

Install:

    brew install dkod-io/tap/dkod
    # or
    curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh

Pure Rust, gitoxide-backed, MIT licensed. v0.2.0 is out now.

I'd love feedback on the ref format, the blame/relink approach, and what
would make this useful on your team.
```

## Maker first comment (post within 5 minutes)

```
Author here. Some background and honest caveats.

Origin: I was running multiple coding agents daily and realized that every
debugging question I had about AI-written code — "why does this function do
X?", "did I actually ask for this?" — was answerable only from the session
transcript, which each tool stores in its own local directory with its own
expiry. Claude Code, for instance, keeps transcripts ~30 days. Git already
solved durable, distributed, access-controlled storage for code; sessions
belong next to the code they produced. So dkod writes them as git objects
under custom refs (`refs/dkod/*` — hidden from `git branch -a`, no noise in
your normal workflow).

Limitations I want to be upfront about:

1. dkod drift v1 is heuristic. It diffs stated intent against the actual
   change surface; it's tuned to under-flag rather than spam you, and I'm
   building a labeled benchmark from public session corpora to publish
   precision numbers. Treat it as a reviewer's attention-director, not a
   verdict.

2. Storage at large-org scale needs the planned rollup index. Sessions
   compress to roughly hundreds of KB each; a 50-dev org generates a lot of
   refs per year. The rollup (one index ref mapping session-id → blob, the
   Gerrit NoteDb pattern), per-session size budgets, `dkod gc`, and lazy
   on-demand fetching are the next milestone. Today it's great for
   individuals and small teams; I'd hold off on a 100-dev rollout until
   that ships.

3. Capture works by adapting each agent's session format, so a breaking
   change upstream can silently break capture for that agent until the
   adapter updates. The team layer monitors capture coverage (% of merged
   PRs with a linked session) precisely because of this.

Roadmap: storage rollup + gc, Agent Trace import/export (the
Cursor/Cognition/Cloudflare/Vercel attribution spec — dkod aims to be its
git-native store), transcript importers to rescue sessions from each tool's
local store before expiry, and an optional evidence-pack export.

Happy to answer anything about the design — especially from people who've
tried to do forensic review of agent output at work.
```

## Posting mechanics (notes for the founder, not part of the post)

- **Timing:** Tue–Thu 9:00–12:00 ET, or Monday 00:00 UTC. Avoid Fri–Sun.
- **Maker comment in 5 minutes.** Stay at the keyboard for the first 3–4
  hours; reply to every substantive comment fast. Speed of author replies is
  the strongest controllable ranking signal.
- **Expect the privacy challenge.** The first hostile comment will probe the
  hosted indexer. Answer is already in the body: transcripts are never
  *stored* outside your git host; the indexer persists metadata only and
  fetches content under the user's token; derived summaries/embeddings are
  per-org opt-in. Do not improvise a stronger claim.
- **Never say** "Entire only supports 2 agents" (false — they support 10),
  never claim clones see sessions automatically (they don't; `dkod init`
  detects and fetches them), never claim regulation requires AI-code
  provenance (it doesn't, yet — say "audit-ready evidence").
- **Assets:** attach/link the 20-second `dkod blame` GIF and the drift card
  screenshot in the maker comment if the submission is a repo link.
- **If it whiffs:** one repost is allowed (different weekday/time). Also email
  hn@ycombinator.com asking for the second-chance pool — moderators do this
  routinely for Show HN posts that died at a bad hour. If it whiffs twice,
  fall to Plan B: r/ClaudeCode series, one influencer video, Changelog News.
