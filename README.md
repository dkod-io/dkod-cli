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

See `docs/plans/2026-05-03-dkod-pivot-design.md` for design context and
`docs/plans/2026-05-03-dkod-cli-v1-implementation.md` for the implementation plan.

MIT licensed.
