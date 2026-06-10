# dkod

Capture every AI agent session into your git repository as a custom git ref.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh
```

(While the repo is private, set `GH_TOKEN` to a GitHub PAT with read access first.)

Or with cargo:

```sh
cargo install --git https://github.com/dkod-io/dkod-cli dkod-cli
```

## Redaction

Captured transcripts are scrubbed at capture time, on by default: known key
formats (AWS, GitHub, OpenAI, Stripe), `ENV=value` assignments, a generic
entropy-based rule for random credentials, plus your own custom patterns.
Every replacement is tallied in a per-session audit count shown by
`dkod show`. Redaction is best-effort, not a guarantee — see
[`docs/redaction.md`](docs/redaction.md) for the full rule set and
limitations.

See `docs/plans/2026-05-03-dkod-pivot-design.md` for design context and
`docs/plans/2026-05-03-dkod-cli-v1-implementation.md` for the implementation plan.

MIT licensed.
