# Team-tier hard collaboration gate — design note

> **WHEN TO SEND / WHERE:** Internal design note, not outbound. Implement on the indexer during months 3–5 (30-day action item #11: "design the Team-tier hard gate"); the gate must be live before Team-tier GA (month 5). NO HUMAN SEND — this is a build spec.

## The line

| | Free forever | Team ($15/seat/mo, $12 annual) |
|---|---|---|
| CLI (capture 7 agents, blame, drift, log/show, redaction) | Everything, MIT, forever | Same — the CLI is never monetized |
| Hosted indexer | **≤5 seats, 1 connected org** | Unlimited seats/repos, org-wide search, weekly drift digest, session-read ACLs, capture-coverage monitor, multi-host (GitHub+GitLab+Gitea) |

The free→Team boundary is a **hard collaboration gate**, Slack/Sentry/Postman
style — not a dashboard upsell. The CLI and the in-repo data plane stay
unconditionally free (strategy: "CLI monetization in any form" is on the
never list; honor-system licensing too).

## Gate triggers

Two events flip an org from free to "paid conversation required":

1. **The 6th seat.** A 6th distinct user authenticates against the same
   org's indexer workspace (seat = unique authenticated user with indexer
   access in a rolling 30 days, counted at auth time — not at invite time,
   so dormant invites don't trip it).
2. **The 2nd org.** The same account/workspace connects a second git
   organization.

Either trigger → the action that crossed the line is **blocked, not
degraded**, with an inline upgrade path. Existing functionality for the
first 5 seats / first org keeps working untouched — the gate stops growth,
it never breaks what's running (no hostage-taking; trust is the brand).

## In-product limit UX

- **Pre-warning at 4/5 seats:** banner in the indexer UI + a line in the
  weekly digest email: "4 of 5 free seats used. The 6th teammate starts a
  Team trial." No nagging before that.
- **At the trigger:** the 6th person's auth screen says exactly what's
  happening — "This workspace has used its 5 free seats. Start a 14-day
  Team trial to add [name], or an admin can swap seats." Two buttons: start
  trial (self-serve, card optional until trial end), notify admin. Never a
  dead end, never "contact sales" for Team tier.
- **Seat swapping stays free:** admins can deactivate a seat to admit
  another (prevents the gate punishing churn on a 5-person team, removes the
  "we got locked out" horror story).
- **Second-org connect:** the connect flow itself presents the gate —
  "Free includes 1 organization. Connect [org] with Team." Same trial path.
- **CLI stays silent.** `dkod` never prints upsells. The gate lives entirely
  in the hosted layer. (One exception: `dkod init` against a 6th-seat-locked
  workspace surfaces the same factual message the web UI shows, once.)
- Every gate screen restates the trust line: transcripts are never stored
  outside your git host — upgrading changes limits, not the data boundary.

## Why a hard gate (the benchmarks)

From the strategy doc's conversion research:

- Freemium org-level conversion benchmarks (ChartMogul 2026): GOOD is 3–5%,
  GREAT is 8–12%. The outliers above that — Slack's famous ~30% — are all
  products with a **hard collaboration gate**, where the team physically
  cannot do the collaborative thing without paying.
- **Conversions cluster within ~7 days of hitting a hard limit**, versus
  90–180 days for value-upsell motions where the team must be persuaded a
  dashboard is worth budget. The gate converts on the team's own growth
  event (a 6th member showing up), which arrives with momentum and an
  internal champion already mid-task.
- AI-native tools only reach the GREAT 15–20% band **with hard gating**;
  the original "15% of activated orgs convert" plan was top-decile and has
  been recalibrated — but the gate is what keeps even the base case
  (~$340 MRR month 6, ~$2k MRR month 12) reachable.
- The anti-pattern is documented in-strategy: Atuin-style activation failure
  (<25 free orgs by month 4) is the thing to watch — the gate only converts
  orgs that exist, so activation metrics gate the gate.

The lever is the gate, not the price: $15 sits inside the proven 2026
corridor (Raycast Teams $12, Postman $19, Copilot Business $19, CodeRabbit
$24, Sentry $26, Greptile $30) and under Macroscope's old $30 anchor.

## The org-flat + seat hybrid hedge

Risk (the Macroscope pivot rationale): in agent-heavy orgs, headcount — and
therefore seats — erodes as agents do more of the work. A pure per-seat
model shrinks with exactly the customers using agents most, i.e. our best
customers.

Hedge, designed now, shipped only on evidence:

- **Structure:** org-flat platform fee (covers the org workspace, search
  index, digest, N bundled seats) + per-seat beyond the bundle. Example
  shape to price-test: $99/org/mo including 10 seats, +$10/seat after.
  Business tier same shape at higher base (SSO/SCIM, audit log, self-hosted
  license key).
- **Trigger to switch:** sustained seat contraction in paying orgs while
  session volume grows (the agent-leverage signature), or win/loss feedback
  that per-seat math killed deals in agent-dense orgs.
- **Compatibility:** the hard gate survives the switch unchanged — the free
  tier stays ≤5 seats/1 org and the trigger stays the 6th seat / 2nd org;
  only what the upgrade *costs* changes shape. Billing code should therefore
  keep "gate logic" and "price plan" as separate modules from day one.
- **Never metered on sessions captured** — that would tax the corpus
  accumulation that is the entire year-1 thesis. Metered pricing applies
  only to LLM-powered features, at cost +10% (the Zed lesson: never
  subsidize inference).

## Instrumentation (so the gate can be judged)

Track from day one: free orgs created, orgs at 4/5 seats, gate-hit events,
gate→trial starts, trial→paid, days-from-gate-to-paid (target: median <7),
and org conversion rate (kill criterion: <3% sustained, or <5 paying teams
90 days post-GA, or <$2k MRR by month 12 → repackage once, then kill the
hosted tier and ride the OSS/evidence wedge).
