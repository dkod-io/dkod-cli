# Blog post draft — the Moonwell case study

> **WHEN TO SEND / WHERE:** dkod.io blog, month 2 (the "amplify" phase, after Show HN). This is the anchor for influencer pitches in the same window — pitch Theo/ThePrimeagen the AI-slop/incident STORY, not the tool. Syndicate: X/Bluesky thread, r/ethdev or security-adjacent subs only if organically relevant, lobste.rs. NEEDS HUMAN TO SEND (publish + pitches).

## Title

```
The $1.78M exploit that was traced through commit trailers
```

Subtitle: *Trailers tell you which model. They can't tell you what it was asked.*

## Post body

```markdown
In February 2026, the Moonwell DeFi protocol was exploited for $1.78M. When
investigators worked backwards from the vulnerable code, the AI provenance
trail they had was a set of commit trailers: `Co-authored-by: Claude`.

That trailer is doing remarkable work in this story, and it's worth pausing
on both halves: it's remarkable that it was there at all, and it's
remarkable how little it says.

## What a trailer can tell you

`Co-authored-by:` trailers are the commit-message convention several agents
attach to their commits. From one, an investigator learns exactly one fact:
an AI model — and which family — participated in authoring this commit.

That's not nothing. In the Moonwell investigation it was enough to establish
that the vulnerable code was AI-assisted, which shaped the entire incident
narrative. One line of metadata, written almost as an afterthought, became
the load-bearing forensic artifact for a seven-figure loss.

## What a trailer cannot tell you

Everything an actual incident review needs next:

- **What was the agent asked?** Was the vulnerable logic explicitly
  requested, or did the agent introduce it while doing something else?
- **What did it reason?** Did the session contain a warning, a considered
  trade-off, a hallucinated assumption about an invariant?
- **What did the human see?** Was the dangerous diff reviewed in-session and
  approved, skimmed, or auto-accepted in a long tool-use run?
- **Where did it drift?** Did the change surface match the stated intent, or
  did the agent touch code nobody asked it to touch?

The trailer answers "which model." The session transcript answers all four —
and in February 2026, for most teams, that transcript was already gone:
agent tools keep session logs locally, per-machine, with retention measured
in days to weeks.

This is the general condition, not a Moonwell-specific failure: the industry
is generating code at agent speed while keeping evidence at scrollback
depth. 66% of developers say AI code being "almost right" is their top
frustration (Stack Overflow 2025); fewer than half review AI-generated code
before committing it (Sonar). The gap between those two numbers is where
incidents come from — and where the evidence to understand them evaporates.

## What the same investigation looks like with sessions in the repo

dkod captures every agent session (7 agents: Claude Code, Codex, Copilot
CLI, Cursor, Factory droid, Gemini CLI, opencode) as git objects under
`refs/dkod/sessions/<id>` — in the repo, on your own git host, with
capture-time redaction on by default. Transcripts are never stored outside
your git host.

Against the vulnerable line, the workflow is:

    dkod blame contracts/src/market.sol
    # line 142 → session 9f3ac1, prompt: "..."

    dkod show 9f3ac1
    # the full session: prompts, reasoning, tool calls, the human's replies

    dkod drift 9f3ac1
    # where the change surface exceeded the stated intent

`dkod blame` survives the rebases and squashes between the agent session and
the audited commit (post-rewrite relinking with a patch-id fallback) — which
matters, because incident archaeology always crosses rewritten history.

Trailers tell you WHICH model. Sessions tell you WHAT it was asked, WHAT it
reasoned, and WHERE it drifted.

## The honest part

dkod could not have helped Moonwell's investigators. It wasn't installed,
so the sessions that produced the vulnerable code were never captured.
No forensic tool reconstructs evidence that was never recorded — anyone
selling retroactive provenance is selling fiction.

That asymmetry is the whole argument. A flight recorder is purchased before
the flight. The day you need session evidence — incident, audit, customer
security review — is precisely the day it's too late to start collecting it.
Every session that runs uncaptured today is a question you permanently
cannot answer tomorrow.

Capture is free, local, and MIT-licensed:

    brew install dkod-io/tap/dkod
    # or
    curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh

Start recording before the interesting flight.
```

## Notes for the founder

- Before publishing, re-verify the Moonwell public reporting (amount, date,
  the trailer detail) and link the best primary source(s) in the post.
  The code snippet file path is illustrative — either genericize it or make
  clear it's a reconstruction, never imply we've seen Moonwell's repo.
- Do NOT drift into "regulation requires this" — the post sells audit-ready
  evidence and incident forensics, not mandates.
- Influencer pitch angle (month 2): the story is "a seven-figure exploit's
  only AI evidence was one commit trailer" — the tool is the third act, not
  the lede.
