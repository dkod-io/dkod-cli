# Reddit series — r/ClaudeCode (3 posts) + r/ClaudeAI adaptation

> **WHEN TO SEND / WHERE:** r/ClaudeCode (~292k members), starting launch week (weeks 3–4), one post per week in the order below. Founder posts under his own established Reddit account — not a fresh throwaway. Each post needs the demo asset ready (GIF / screenshot) before posting. NEEDS HUMAN TO SEND.

## Cadence + authenticity rules (apply to all three)

- **One post per week, max.** Reddit mods and users punish tool-spam; three
  posts in a week reads as a marketing campaign, three weeks apart reads as
  a builder sharing progress.
- Lead with the demo and the problem, never the product name. Disclose
  "I built this" in the first paragraph — undisclosed self-promo gets removed.
- Reply to every comment in the first 6 hours. Answer criticism technically,
  never defensively. Upvote nothing of your own from alts (obviously).
- In the week between posts, participate in 3–5 unrelated threads in the sub
  (genuinely — answer questions about hooks, transcripts, agents). Account
  history is the first thing skeptics check.
- Honest claims only: 7 agents captured; transcripts are never stored outside
  your git host; teammates run `dkod init` to fetch sessions (clones do NOT
  see them automatically); drift v1 is heuristic.

---

## Post 1 (week 1 of series) — the blame post

**Title:**

```
I built git blame for AI sessions — find out which prompt wrote this line
```

**Body:**

```
I built this, so flagging that up front.

Demo first: [20-second GIF — `dkod blame src/auth.rs`, line resolves to the
session + the exact prompt that produced it]

The itch: I'd come back to AI-written code a week later and have no idea
what I'd actually asked for. git blame told me the commit; the commit told
me nothing about the conversation. The transcript that *did* explain it was
sitting in `~/.claude/projects` waiting to expire.

So dkod captures every session (Claude Code plus Codex, Copilot CLI, Cursor,
Factory droid, Gemini CLI, opencode — 7 agents) as git objects under
`refs/dkod/sessions/<id>` in your own repo. Then:

- `dkod blame <file>` — line → session → prompt. Survives rebases via a
  post-rewrite hook with a patch-id fallback, because agent-era branches get
  rewritten constantly.
- `dkod log` / `dkod show` — browse and replay sessions.

Privacy, since these are transcripts of you talking about your codebase:
everything lives in YOUR repo on YOUR git host — transcripts are never
stored outside your git host. Capture-time redaction is on by default, so
secrets get scrubbed before anything is written.

Install:

    brew install dkod-io/tap/dkod
    # or
    curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh

MIT, Rust, repo: https://github.com/dkod-io/dkod-cli

Would love to hear how you currently answer "what did I ask for here?" a
month after the fact — or whether you just re-prompt and move on.
```

**First comment (post immediately):**

```
A couple of implementation notes people usually ask about:

- The refs are custom (`refs/dkod/*`), so they don't show up in
  `git branch -a` or clutter normal git UX. Teammates don't get them on
  clone automatically — they run `dkod init` in the repo and it detects +
  fetches existing sessions.
- Capture is per-agent adapters reading each tool's native session format,
  so it works with your existing setup — no proxy, no wrapper shell.
- Yes, sessions add storage to the repo. Fine at individual/small-team
  scale; a rollup index + gc for big-org scale is the next milestone and
  I'm honest in the README about not rolling it out to 100-dev orgs yet.
```

---

## Post 2 (week 2 of series) — the transcript-rescue post

**Title:**

```
Your Claude Code transcripts get deleted after 30 days — here's how I archive mine in git
```

**Body:**

```
Disclosure: I built the tool at the bottom, but the first half is just the
mechanics, which work without it.

Claude Code stores your session transcripts as JSONL under
`~/.claude/projects/<munged-repo-path>/`. By default they're cleaned up
after ~30 days (`cleanupPeriodDays` in settings). If you've ever wanted to
revisit how you solved something two months ago — the prompts, the wrong
turns, the fix — odds are it's already gone.

Option 1, zero tools: raise `cleanupPeriodDays`, and/or cron a copy of
`~/.claude/projects` somewhere safe. Works, but it's a pile of JSONL with
no link to the code it produced, and it's only on one machine.

Option 2, what I do now: I archive sessions INTO the repo they belong to,
as git objects under `refs/dkod/sessions/<id>`. That gets you, for free
from git: durability, every-machine availability, your git host's access
control, and the session permanently attached to the code it wrote. My
tool (dkod, MIT/Rust) does the capture automatically and has an importer
to rescue what's currently sitting in `~/.claude/projects` before it
expires. It also captures 6 other agents if you mix tools.

Transcripts are never stored outside your git host, and capture-time
redaction is on by default — worth saying because archiving transcripts
forever is exactly when a leaked key would hurt.

    brew install dkod-io/tap/dkod

Repo: https://github.com/dkod-io/dkod-cli

Either way: go check the mtime on your `~/.claude/projects` folder. People
consistently don't realize the 30-day cleanup exists until the session
they need is gone.
```

**First comment:**

```
FAQ from previous threads on transcript expiry:

- "Does raising cleanupPeriodDays have downsides?" Disk growth, that's it.
  But it stays local to one machine and unlinked from your repos.
- "Why in the repo and not a notes app?" Because the question you'll have
  later is code-shaped: "what produced THIS function?" In-repo sessions
  make that answerable with `dkod blame <file>` — line to prompt.
- "Do my teammates see my sessions if they clone?" No — custom refs don't
  transfer on clone. They'd run `dkod init`, which detects and fetches
  them. Sharing is deliberate, not accidental.
```

---

## Post 3 (week 3 of series) — the drift post

**Title:**

```
dkod drift: catch your agent doing more than you asked
```

**Body:**

```
I built this — third and last post in a short series (blame, transcript
rescue, and now this).

Screenshot: [drift card — intent: "fix the failing date parser test";
flagged: also modified retry logic in http/client.rs, added a dependency]

We've all had the session where you ask for a one-line fix and get a
refactor. Stack Overflow's 2025 survey has 66% of devs calling "almost
right" AI code their top frustration — and the "almost" is usually the part
you didn't ask for. Less than half of AI-generated code gets reviewed
before commit (Sonar), so the extras just... land.

`dkod drift` compares what you asked for against what the agent actually
changed, per session, and prints a card flagging where the change surface
exceeded the stated intent. It works because dkod already captures the full
session (7 agents) into your repo as git refs — so intent and diff are both
right there to compare.

Honesty section, because this sub can smell overclaiming:

- Drift v1 is heuristic. It's tuned to under-flag (quiet > noisy), and I'm
  building a labeled benchmark to publish precision numbers. It's a
  reviewer's attention-director, not a judge.
- It can't catch drift in sessions it never captured. Capture first, drift
  second.

    brew install dkod-io/tap/dkod

Repo: https://github.com/dkod-io/dkod-cli

What's the worst "I asked for X and got X+Y" you've had? Genuinely
collecting cases for the benchmark.
```

**First comment:**

```
For the benchmark: if you have a public/shareable session where the agent
clearly exceeded the brief, I'll take it — building a labeled set of
50–100 sessions to measure drift precision before I make any accuracy
claims. Happy to credit contributors in the repo.

And for reviewers rather than authors: the team-facing version of this is
a weekly drift digest across the org's sessions — that's the part that
turns "huh, neat" into less review burden. Early; feedback shapes it.
```

---

## r/ClaudeAI adaptation note (+1)

r/ClaudeAI (~911k members) is broader and less terminal-native than
r/ClaudeCode. Adapt **Post 2 (transcript rescue)** for it — it's the most
universal hook (loss aversion, no CLI sophistication needed):

- Retitle: `PSA: Claude Code deletes your session transcripts after 30 days
  — how to keep yours`
- Lead with the PSA/mechanics half; move the dkod section lower and shorter.
- Cut the `refs/dkod` plumbing detail; keep "archived into the repo, never
  stored outside your git host" as one sentence.
- Post it 1–2 weeks AFTER the r/ClaudeCode version, never the same week, and
  never cross-post directly (subs notice and it reads as a campaign).
- Same authenticity rules: disclosure up top, reply for 6 hours, account must
  have organic history in the sub.
