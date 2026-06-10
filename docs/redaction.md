# Redaction

dkod pushes AI agent transcripts into shared repos, so capture-time redaction
is on by default. Every session is scrubbed in `finalize_session` — before it
is ever written to a git blob — by replacing secret-shaped text with
`[REDACTED:<rule>]` markers. The original secret is never stored.

Redaction walks every text-bearing field of a session: the prompt summary,
user / assistant / reasoning message content, tool outputs, and every string
inside tool-input JSON (recursively).

## What gets redacted

### Builtin rules (all enabled by default)

| Rule | Matches | Replacement |
| --- | --- | --- |
| `builtin:aws` | AWS access key ids (`AKIA…`, `ASIA…`, `AROA…`, `ABIA…`, `ACCA…` + 16 chars) | `[REDACTED:aws]` |
| `builtin:github_token` | GitHub classic PATs (`ghp_`, `gho_`, `ghu_`, `ghs_`) and fine-grained PATs (`github_pat_…`) | `[REDACTED:github_token]` |
| `builtin:openai_key` | OpenAI API keys (`sk-…`, `sk-proj-…`) | `[REDACTED:openai_key]` |
| `builtin:stripe` | Stripe secret keys (`sk_live_…`, `sk_test_…`) | `[REDACTED:stripe]` |
| `builtin:env_assignment` | `UPPER_CASE=value` assignments — the value is redacted, the variable name is kept | `NAME=[REDACTED:env_assignment]` |
| `builtin:entropy` | Generic high-entropy credentials (see below) | `[REDACTED:entropy]` |

### `builtin:entropy` — generic credential detection

The shape-specific rules above only catch known key formats. The entropy rule
catches the long tail: random API keys, signing secrets, bearer tokens, and
other machine-generated credentials with no recognizable prefix.

How it works:

- Candidate tokens are runs of base64-style characters
  (`[A-Za-z0-9+/=_-]`) at least **24 characters** long.
- A candidate is redacted when its per-character Shannon entropy is at least
  **4.2 bits**. Hex tops out at 4.0 bits (log2 16), so git SHAs and hex
  digests always stay below the threshold; random base64 sits around 4.6–5.6
  bits at these lengths; English text sits around 3–4.
- Explicit skip rules guard the common false-positive classes:
  - all-hex strings of exactly 40 or 64 chars (git SHA-1 / SHA-256 object
    ids, which legitimately appear in transcripts)
  - all-digit runs (ids, timestamps, order numbers)
  - tokens containing two or more `/` characters (overwhelmingly file paths,
    not base64)

### Custom patterns

Add your own regexes in `.dkod/config.toml`; matches become
`[REDACTED:custom]`:

```toml
[redact]
custom = ["(?i)internal-token-[a-z0-9]{16}"]
```

To change the rule set (or disable redaction entirely — not recommended):

```toml
[redact]
enabled = true
patterns = [
  "builtin:aws",
  "builtin:github_token",
  "builtin:openai_key",
  "builtin:stripe",
  "builtin:env_assignment",
  "builtin:entropy",
]
```

## Audit count

Every replacement increments the session's `redaction_count` field, stored in
the session blob. `dkod show <id>` prints `redactions: N` in the metadata
block when N is greater than zero, so you can see at a glance that the
redactor fired before a transcript was shared. Sessions captured before this
field existed read back with a count of 0.

## Limitations — read this before pushing sensitive repos

Redaction is **best-effort, not a guarantee**:

- Secrets in unusual encodings (split across lines, embedded in binary
  output, encoded twice, shorter than 24 chars with no known prefix) can
  survive redaction.
- The entropy rule is statistical; a low-entropy passphrase or a secret
  diluted inside a longer low-entropy run will not trip it.
- Custom formats your organization uses are invisible to the builtins — add
  custom patterns for them.

Review captured sessions (`dkod show`) before pushing refs from repositories
that handle sensitive material. A deployment mode that pushes sessions to a
separate, sessions-only remote is on the roadmap.
