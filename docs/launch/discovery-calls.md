# Discovery calls — the 20-call demand-authenticity script

> **WHEN TO SEND / WHERE:** Begin in weeks 1–4 and run through month 2 (30-day action item #12). 20 structured calls, 25–30 minutes each, video or phone. Sourced via warm intros, LinkedIn, GitLab/Bedrock community spaces, and inbound from the launch posts. This is the gate in front of ALL compliance/evidence-pack messaging — do not market "audit-ready evidence" until this completes. NEEDS HUMAN TO RUN (founder-led calls; do not delegate or automate).

## Purpose and the kill rule

The strategy's evidence-pack gate: before building `dkod report` marketing or
any compliance-flavored positioning, verify the demand is real and current.

**Kill rule: if fewer than 5 of the 20 calls confirm a real
auditor/insurer/acquirer ask for AI-code evidence in the past 12 months,
drop compliance messaging entirely** and keep the product framed on review
burden + incident forensics only. No softening, no "but they seemed
interested." A hypothetical future ask does not count; only "it happened,
here's roughly when and who asked."

Scoring: each call ends with a binary mark — CONFIRMED (a named class of
external party actually asked, past 12 months) or NOT CONFIRMED. Track in a
simple sheet: date, role, org type, mark, verbatim quote.

## Target profile

Engineering leaders at organizations that are simultaneously:

1. **Compliance-bound** — SOC 2 (Type II especially), ISO 27001/42001,
   or sector-regulated (fintech, health, defense-adjacent, EU-exposed); and
2. **Actively using AI coding agents** — 2+ agents in real use, or a formal
   rollout in flight; and
3. **Storage-sensitive** — refuse persistent third-party storage of code
   while permitting contracted inference (Bedrock/Vertex in their own
   account, ZDR addenda). GitLab Self-Managed and GitHub Enterprise Server
   shops are priority cells.

Seniority: the person who would be HANDED the auditor's request — VP Eng,
Head of Platform, Director of Engineering, security/compliance lead who owns
the SOC 2 evidence calendar. Not ICs, not pure-GRC people with no eng scope.

## The 8 questions (in order)

Open with: "I'm researching how teams handle evidence and review for
AI-written code — not selling on this call; I want 25 minutes of how it
actually works at your org."

1. **Agent reality check.** Which AI coding agents are in real use on your
   teams today, and roughly what share of merged code do they touch?
2. **Review pain.** When an agent's change is "almost right," how does that
   get caught today — and who eats the time? What does your review process
   do differently for AI-authored changes, if anything?
3. **Incident recall.** Has AI-written code been implicated in an incident
   or near-miss in the past year? When you investigated, what evidence did
   you actually have about what the agent was asked and did?
4. **THE GATE QUESTION (verbatim, score this one):** In the past 12 months,
   has an auditor, cyber-insurer, customer security review, or acquirer's
   diligence team actually asked you for evidence about AI-generated code —
   its provenance, review, or approval? If yes: who asked, what exactly did
   they want, and what did you give them?
5. **Governance posture.** Did your agent rollout go through any approval
   body (security review, AI governance board, CISO sign-off)? What evidence
   or controls did THEY require to say yes?
6. **Data boundary.** What's your policy on tools that store code or
   transcripts with a third party? Has that policy actually blocked a
   purchase in the past year — which tool, and what would have changed the
   answer?
7. **Status quo cost.** If you had to produce "show me what the AI was asked
   and what it changed" for one specific merged PR from 3 months ago, could
   you? How long would it take, and who would do it?
8. **Money question (last, deliberately).** Your org already pays for
   eng-intelligence or code-review tooling — what does it pay per dev per
   month, and where would per-line AI provenance rank against what that
   money buys today?

Close: "If I build the evidence-pack piece, can I show you a prototype?" —
a yes here is the design-partner pipeline; it does NOT count toward the gate.

## Interview discipline

- Never pitch before question 8. The moment you pitch, every answer after is
  contaminated by politeness.
- Past tense beats future tense: "has asked" counts, "would probably ask"
  doesn't (The Mom Test rule — people are generous about hypotheticals).
- Get verbatim quotes for question 4; they're the marketing copy if the gate
  passes and the kill evidence if it doesn't.

## 20-name target-type list (roles/org types — fill in real names during sourcing)

GitLab Self-Managed cell (5):
1. VP Engineering, mid-size fintech on GitLab Self-Managed, SOC 2 Type II
2. Head of Platform, healthcare SaaS on GitLab Self-Managed (HIPAA + SOC 2)
3. Director of Eng, EU bank or bank-adjacent on GitLab Self-Managed (DORA register pain)
4. Engineering lead, gov-contractor dev shop on GitLab Self-Managed
5. DevSecOps lead, 50–100-dev manufacturer running GitLab + Claude Code rollout

GitHub Enterprise Server / Copilot-restricted cell (4):
6. VP Eng, insurance carrier on GHES with a BYO-agent policy
7. Platform director, telco on GHES mid agent-rollout approval
8. Head of Developer Experience, large retailer on GHES, ISO 27001
9. Eng director, defense-adjacent software firm on GHES (air-gap leanings)

ZDR / Bedrock / Vertex cell (4):
10. CTO, Series B–C devtools/AI startup on Anthropic ZDR addendum
11. Head of AI Platform, enterprise running Claude via Bedrock in own account
12. VP Eng, data-infrastructure company on Vertex with code-egress restrictions
13. Security engineering lead at an org that rejected a SaaS code tool on storage grounds in the past year

Regulated / audited verticals (4):
14. Eng lead, DeFi protocol or smart-contract team with external audit cadence (Moonwell-adjacent profile)
15. VP Eng, payments company under PCI + SOC 2 with agents in production use
16. Engineering manager, medtech firm pursuing ISO 42001
17. Director of Eng, public-company SaaS with annual SOC 2 + customer security questionnaires

Governance / diligence adjacents (3):
18. AI governance board member (eng-side) at an enterprise that formally approved an agent rollout
19. Compliance/GRC lead who personally ran the last SOC 2 audit at an agent-using org
20. Corp-dev / technical-diligence lead who has run an acquisition code review in the past 18 months
