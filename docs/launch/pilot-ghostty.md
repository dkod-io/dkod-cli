# Ghostty / Mitchell Hashimoto pilot outreach — email/DM draft

> **WHEN TO SEND / WHERE:** Week 1 of the launch sequence, alongside the tldraw outreach. Email preferred (Mitchell publishes contact info); X/Bluesky DM as fallback. Founder sends under his own name. Prerequisite: zero-install web session viewer live. NEEDS HUMAN TO SEND.

## Context (do not paste)

Mitchell's stated position on AI contributions is roughly "anti-idiot, not
anti-AI" — if you're going to use AI, you'd better be good, and you'd better
have driven it. That's exactly the distinction a session transcript makes
legible. Keep it short. No flattery padding. One concrete offer, opt-in only.

## Subject

```
Verifiable agent sessions for Ghostty PRs — opt-in, carrot only
```

## Body

```
Hi Mitchell,

Your line on AI contributions — anti-idiot, not anti-AI — names a real
problem: disclosure trailers tell you a PR used AI, but not whether the
author drove the agent or just shipped its first answer. Disclosure exists
to calibrate review attention, and right now there's nothing to calibrate
against.

I built dkod (MIT, Rust: https://github.com/dkod-io/dkod-cli). It captures
AI agent sessions — Claude Code, Codex, Copilot CLI, Cursor, and 3 others —
as git objects in the contributor's own fork. The session is the AI-driver's
flight recorder: the prompts, the iterations, the places the human caught
and corrected the agent. A contributor attaches a link; a reviewer opens it
in a read-only web viewer with nothing installed.

The offer, for Ghostty, entirely opt-in:

  Session attached ⇒ priority review.

Nobody is required to do anything. Hand-written PRs are untouched. AI PRs
without a session go through the normal queue. The only change is a fast
lane for contributors who can show their work — cheap for the competent,
expensive for the careless, which I think is the filter you're actually
after.

I'll build whatever the workflow needs (label/bot, viewer tweaks) to your
spec. If it's a "no," a one-word reply is fine and I won't follow up.

Haim
https://dkod.io
```

## Notes for the founder

- Keep the email under ~220 words as drafted; resist additions.
- If he replies with interest, propose async-first (he's stated preferences
  for async); offer a 15-minute call only if he asks.
- If he pushes on privacy: sessions are in the contributor's fork; transcripts
  are never stored outside the contributor's git host; capture-time redaction
  is on by default.
- One follow-up maximum, after 2 weeks, only if Show HN traction gives a new
  reason to write ("this landed on HN; offer stands").
