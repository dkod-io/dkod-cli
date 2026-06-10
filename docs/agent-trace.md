# Agent Trace export

`dkod export agent-trace` emits stored dkod sessions as
[Agent Trace](https://github.com/cursor/agent-trace) records — the open,
vendor-neutral format for recording AI contributions to codebases, started
under the `cursor` GitHub org with participation from Cursor, Cognition,
Cloudflare, Vercel, and git-ai.

The spec is deliberately storage-agnostic: it defines the *shape* of an
attribution record, not where records live. dkod's position is that records
(and the full session transcripts behind them) belong in the repository —
dkod is the git-native Agent Trace store.

## Spec version

Implemented against **Agent Trace v0.1.0**, `cursor/agent-trace` repo commit
[`2754f077`](https://github.com/cursor/agent-trace/commit/2754f077f3e50c1fb5088183f5c9362077cc8ca1)
(2026-02-06, the repo head at implementation time). The spec repo ships its
schema as `schemas.ts` (plus the JSON Schema rendering in its README), not as
standalone `.json` schema files, so dkod's tests validate the required fields
structurally rather than vendoring a schema file.

## Usage

```sh
dkod export agent-trace                  # all sessions -> ./agent-traces/<id>.json
dkod export agent-trace <id>             # one session  -> ./agent-traces/<id>.json
dkod export agent-trace <id> --out dir/  # custom output directory
dkod export agent-trace <id> --out -     # one record to stdout
```

One JSON file per session, named `<session-id>.json`. `--out -` requires a
session id so stdout is always a single valid JSON document.

## Field mapping

One trace record per session.

| Agent Trace field | dkod source | Notes |
|---|---|---|
| `version` | constant `"0.1.0"` | spec version (required) |
| `id` | `Session.id` | dkod session ids are UUID v7 — valid spec UUIDs (required) |
| `timestamp` | `Session.created_at` | epoch seconds rendered as RFC 3339 UTC (required) |
| `vcs.type` | constant `"git"` | dkod only stores git repos |
| `vcs.revision` | `Session.commits[0]` | the newest commit the session produced; the **full** commit list is in `metadata["io.dkod"].commits`. Omitted when the session produced no commits |
| `tool.name` / `tool.version` | constant `"dkod"` + crate version | the tool that *emitted the record* — see "Interpretation choices" |
| `files[].path` | `Session.files_touched` | one file entry per touched path, relative to the repo root |
| `files[].conversations[].contributor.type` | constant `"ai"` | dkod only captures agent sessions |
| `files[].conversations[].ranges` | constant `[]` | always empty — see "What is intentionally omitted" |
| `metadata["io.dkod"].agent` | `agent_label(Session.agent)` | e.g. `claude_code`, `codex` |
| `metadata["io.dkod"].session_ref` | `refs/dkod/sessions/<id>` | in-repo pointer to the full transcript |
| `metadata["io.dkod"].prompt_summary` | `Session.prompt_summary` | |
| `metadata["io.dkod"].prompts` | user messages from `Session.messages` | in order |
| `metadata["io.dkod"].commits` | `Session.commits` | every commit attributed to the session |
| `metadata["io.dkod"].created_at` | `Session.created_at` | raw epoch seconds |
| `metadata["io.dkod"].duration_ms` | `Session.duration_ms` | |
| `metadata["io.dkod"].redaction_count` | `Session.redaction_count` | capture-time redaction audit count |

Vendor metadata is keyed by reverse-domain notation per the spec convention:
`"io.dkod"`.

## What is intentionally omitted (and why)

- **Line ranges (`ranges`)** — the spec supports line-level attribution
  (`start_line`/`end_line`, 1-indexed). dkod stores **file + commit**
  granularity, not line ranges. The spec schema places no minimum length on
  `ranges`, so dkod emits an honest empty array instead of fabricating
  whole-file line spans it cannot verify. (Line-level questions are answered
  in-repo by `dkod blame`, which resolves lines through commit/patch-id
  links at query time rather than storing spans.)
- **`contributor.model_id`** — optional; follows the models.dev
  `provider/model` convention. dkod records which agent CLI ran, not the
  underlying model id, so it is omitted rather than guessed.
- **`conversations[].url`** — optional; dkod sessions live in the repo
  itself, not behind a URL. The in-repo pointer is
  `metadata["io.dkod"].session_ref`.
- **Agent version** — the spec's `tool` object requires both `name` and
  `version`, and dkod does not capture the agent's version. Emitting the
  agent's name with dkod's version would mislabel the record, so `tool`
  identifies dkod (the record producer) and the agent is named in
  `metadata["io.dkod"].agent`.
- **`related`** — optional; no mapping source in a dkod session.

## Example record

```json
{
  "version": "0.1.0",
  "id": "0192f8e2-7b3a-7000-8a3e-000000000001",
  "timestamp": "2025-01-01T00:00:00Z",
  "vcs": { "type": "git", "revision": "1111111111111111111111111111111111111111" },
  "tool": { "name": "dkod", "version": "0.2.1" },
  "files": [
    {
      "path": "src/auth.rs",
      "conversations": [
        { "contributor": { "type": "ai" }, "ranges": [] }
      ]
    }
  ],
  "metadata": {
    "io.dkod": {
      "agent": "claude_code",
      "session_ref": "refs/dkod/sessions/0192f8e2-7b3a-7000-8a3e-000000000001",
      "prompt_summary": "fix the auth bug",
      "prompts": ["fix the auth bug"],
      "commits": ["1111111111111111111111111111111111111111"],
      "created_at": 1735689600,
      "duration_ms": 12345,
      "redaction_count": 0
    }
  }
}
```

## Implementation

The mapping is a pure function in `dkod-core`
(`crates/dkod-core/src/trace.rs`, `dkod_core::trace::session_to_trace`);
the CLI subcommand (`crates/dkod-cli/src/cmd/export.rs`) is a thin wrapper
that resolves sessions and handles file/stdout output. Import (ingesting
Agent Trace records produced by other tools) is not implemented yet.
