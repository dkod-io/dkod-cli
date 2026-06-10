# tldraw pilot outreach — GitHub comment draft

> **WHEN TO SEND / WHERE:** Week 1 of the launch sequence (pilot outreach starts before Show HN). Post as a comment on tldraw/tldraw issue #7695 (their external-PR policy thread), or a fresh discussion if the maintainers prefer — read the room in the thread first. Founder posts under his own GitHub account. Prerequisite: the zero-install web session viewer must be live so the link in the draft resolves. NEEDS HUMAN TO SEND.

## Context (do not paste)

tldraw currently auto-closes ALL external PRs, and their own wording calls it
"temporary pending better platform tooling." That sentence is the opening:
they are not anti-contribution, they are missing tooling to triage AI-slop
PRs. We are offering to be that tooling — as a carrot ("a session reopens
your PR"), never as a mandate. Do not ask them to require anything of
contributors. Offer to build the workflow with them.

## Comment draft

```markdown
Hi tldraw team — I read the policy note that external PRs are closed
"temporary pending better platform tooling," and I'm building tooling in
exactly that gap, so I wanted to offer it rather than watch from the
sidelines.

**The idea: PRs reopen if a verifiable agent session is attached.**

I maintain [dkod](https://github.com/dkod-io/dkod-cli) (MIT, Rust), which
captures AI coding-agent sessions — Claude Code, Codex, Copilot CLI, Cursor,
Factory droid, Gemini CLI, opencode — as git objects in the contributor's own
fork (`refs/dkod/sessions/<id>`). A contributor who used an agent can attach
the session link to their PR. What you'd see, without installing anything,
via a read-only web viewer:

- the actual prompts the contributor gave the agent
- every iteration — what the agent tried, what failed, what got reworked
- the human corrections along the way (which is usually the fastest signal
  separating someone who drove the agent from someone who pasted its first
  output)

That last point is the one that matters for slop triage: a real session is
nearly free for an honest contributor to attach and nearly impossible for a
drive-by slop farmer to fake convincingly.

**What I am explicitly NOT proposing:**

- not asking you to mandate dkod, or sessions, or anything, for anyone
- not asking contributors who code by hand to do anything different
- no install on your side — review happens through a shareable read-only
  viewer link: <https://dkod-indexer.fly.dev/v/github/dkod-io/dkod-cli/0196f8e2-2222-7000-8000-00000000demo> (zero-install, live example — renders the session
  read-only)

The framing is purely a carrot: a closed-by-policy PR gets a path back to
human review if the author can show their work. Your default stays exactly
as it is.

If this is interesting even in principle, I'd genuinely like to build the
workflow with you rather than guess at it — e.g. a bot label
(`session-attached`) that flags reopen candidates, what the viewer needs to
surface for your reviewers, what would make verification trustworthy enough.
Happy to do all the building; you'd set the bar.

And if the answer is "not now," no hard feelings — I'll keep an eye on the
policy and check back when the timing's better.
```

## Notes for the founder

- Verify the viewer URL before sending; swap in the real link.
- If a maintainer responds with interest, the next step is a 30-minute call
  or async thread to spec the bot/label workflow — offer both.
- If they raise privacy: sessions live in the *contributor's* fork; tldraw
  stores nothing and runs nothing.
- Do not name-drop other pilot targets in the thread.
