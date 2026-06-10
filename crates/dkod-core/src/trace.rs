//! Agent Trace export — maps a stored dkod [`Session`] onto the open
//! [Agent Trace](https://github.com/cursor/agent-trace) attribution format
//! (the `cursor/agent-trace` spec, backed by Cursor, Cognition, Cloudflare,
//! Vercel, and git-ai).
//!
//! Implemented against **spec v0.1.0**, repo commit
//! `2754f077f3e50c1fb5088183f5c9362077cc8ca1` (2026-02-06). The spec defines
//! the shape of a trace record (`schemas.ts` / the JSON Schema in the spec
//! README) but is deliberately storage-agnostic; dkod's position is the
//! git-native store for these records.
//!
//! # Mapping (one trace record per session)
//!
//! | spec field | source | notes |
//! |---|---|---|
//! | `version` | [`SPEC_VERSION`] | required, semver |
//! | `id` | `Session.id` | dkod ids are UUID v7 — valid spec UUIDs |
//! | `timestamp` | `Session.created_at` | epoch seconds → RFC 3339 UTC |
//! | `vcs` | `Session.commits[0]` | newest session commit; omitted when the session produced no commits |
//! | `tool` | `dkod` + crate version | the tool that *emitted the record* (see below) |
//! | `files[].path` | `Session.files_touched` | one entry per touched file |
//! | `files[].conversations[].contributor.type` | constant `"ai"` | dkod only records agent sessions |
//! | `files[].conversations[].ranges` | `[]` | **always empty** — see "Granularity" |
//! | `metadata["io.dkod"]` | session fields | agent, prompts, commit list, etc. |
//!
//! # Interpretation choices (documented, not invented)
//!
//! * **Granularity.** The spec supports line-level attribution via `ranges`
//!   (`start_line`/`end_line`); dkod stores file + commit granularity, not
//!   line ranges. `ranges` has no minimum-length constraint in the spec
//!   schema, so we emit an honest empty array rather than fabricating
//!   whole-file line spans we cannot verify.
//! * **`tool`.** The spec's `Tool` object requires both `name` and
//!   `version`. dkod does not capture the *agent's* version, so emitting
//!   the agent name with dkod's version would mislabel the record. We emit
//!   `tool = { name: "dkod", version: <dkod-core crate version> }` — the
//!   tool that produced the trace — and identify the contributing agent in
//!   `metadata["io.dkod"].agent` (plus `contributor.type = "ai"`).
//! * **`contributor.model_id`.** Optional in the spec, follows the
//!   models.dev `provider/model` convention. dkod records the agent CLI,
//!   not the underlying model id, so it is omitted.
//! * **`conversations[].url`.** Optional; dkod sessions live in-repo (no
//!   URL), so it is omitted. The in-repo pointer is
//!   `metadata["io.dkod"].session_ref`.
//! * **`vcs.revision`.** The spec allows a single revision per record; a
//!   dkod session may produce several commits. We emit the newest commit
//!   (`commits[0]`, head-most by construction of the capture walk) and the
//!   full list under `metadata["io.dkod"].commits`.
//! * **Vendor metadata key.** Reverse-domain per the spec convention:
//!   `"io.dkod"`.

use crate::{agent_label, refs, Message, Session};
use anyhow::{Context, Result};
use serde::Serialize;

/// Agent Trace spec version this module implements.
pub const SPEC_VERSION: &str = "0.1.0";

/// `cursor/agent-trace` repo commit the implementation was pinned against.
pub const SPEC_COMMIT: &str = "2754f077f3e50c1fb5088183f5c9362077cc8ca1";

/// Reverse-domain vendor key for dkod data inside `metadata`.
pub const DKOD_METADATA_KEY: &str = "io.dkod";

/// Top-level Agent Trace record (spec `TraceRecordSchema`).
#[derive(Debug, Clone, Serialize)]
pub struct TraceRecord {
    /// Spec version, semver (`^[0-9]+\.[0-9]+\.[0-9]+$`). Required.
    pub version: String,
    /// UUID of the record. Required. dkod reuses the session's UUID v7.
    pub id: String,
    /// RFC 3339 datetime. Required.
    pub timestamp: String,
    /// Version-control metadata. Optional in the spec; omitted when the
    /// session produced no commits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs: Option<Vcs>,
    /// Tool that produced this record. Optional in the spec.
    pub tool: Tool,
    /// Per-file attribution. Required (may be empty).
    pub files: Vec<TraceFile>,
    /// Vendor-specific data, keyed by reverse-domain notation.
    pub metadata: serde_json::Value,
}

/// Spec `VcsSchema`.
#[derive(Debug, Clone, Serialize)]
pub struct Vcs {
    /// One of `git` / `jj` / `hg` / `svn`. dkod always emits `git`.
    #[serde(rename = "type")]
    pub vcs_type: String,
    /// VCS-specific revision identifier (a commit SHA for git).
    pub revision: String,
}

/// Spec `ToolSchema` — both fields required.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub version: String,
}

/// Spec `FileSchema`.
#[derive(Debug, Clone, Serialize)]
pub struct TraceFile {
    /// Path relative to the repository root.
    pub path: String,
    pub conversations: Vec<Conversation>,
}

/// Spec `ConversationSchema`. `url` and `related` are optional and never
/// emitted by dkod (sessions live in-repo, not behind a URL).
#[derive(Debug, Clone, Serialize)]
pub struct Conversation {
    pub contributor: Contributor,
    /// Required by the spec; always empty for dkod (file-level granularity
    /// only — see the module docs).
    pub ranges: Vec<Range>,
}

/// Spec `ContributorSchema`. `model_id` is optional and never emitted
/// (dkod records the agent CLI, not the underlying model).
#[derive(Debug, Clone, Serialize)]
pub struct Contributor {
    /// One of `human` / `ai` / `mixed` / `unknown`. dkod always emits `ai`.
    #[serde(rename = "type")]
    pub contributor_type: String,
}

/// Spec `RangeSchema` (1-indexed line span). Defined for spec completeness;
/// dkod never constructs ranges because it does not store line granularity.
#[derive(Debug, Clone, Serialize)]
pub struct Range {
    pub start_line: u64,
    pub end_line: u64,
}

/// Map a stored dkod session onto one Agent Trace record.
pub fn session_to_trace(session: &Session) -> TraceRecord {
    let prompts: Vec<&str> = session
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();

    TraceRecord {
        version: SPEC_VERSION.to_string(),
        id: session.id.clone(),
        timestamp: rfc3339_utc(session.created_at),
        vcs: session.commits.first().map(|sha| Vcs {
            vcs_type: "git".into(),
            revision: sha.clone(),
        }),
        tool: Tool {
            name: "dkod".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        files: session
            .files_touched
            .iter()
            .map(|path| TraceFile {
                path: path.clone(),
                conversations: vec![Conversation {
                    contributor: Contributor {
                        contributor_type: "ai".into(),
                    },
                    ranges: Vec::new(),
                }],
            })
            .collect(),
        metadata: serde_json::json!({
            DKOD_METADATA_KEY: {
                "agent": agent_label(&session.agent),
                "session_ref": refs::session_ref(&session.id),
                "prompt_summary": session.prompt_summary,
                "prompts": prompts,
                "commits": session.commits,
                "created_at": session.created_at,
                "duration_ms": session.duration_ms,
                "redaction_count": session.redaction_count,
            }
        }),
    }
}

/// Serialize a session's Agent Trace record as pretty-printed JSON.
pub fn trace_json(session: &Session) -> Result<String> {
    serde_json::to_string_pretty(&session_to_trace(session)).context("serialize trace record")
}

/// Format Unix epoch seconds as an RFC 3339 UTC datetime
/// (`YYYY-MM-DDTHH:MM:SSZ`). Hand-rolled (Howard Hinnant's civil-from-days
/// algorithm) so the crate stays free of a date dependency.
pub fn rfc3339_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs = epoch_secs.rem_euclid(86_400);

    // days since 1970-01-01 -> civil date (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2);

    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Agent, Message, Session};

    fn fixture_session() -> Session {
        Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000001".into(),
            agent: Agent::ClaudeCode,
            created_at: 1_735_689_600, // 2025-01-01T00:00:00Z
            duration_ms: 12_345,
            prompt_summary: "fix the auth bug".into(),
            messages: vec![
                Message::user("fix the auth bug"),
                Message::reasoning("inspect token expiry"),
                Message::assistant("done"),
                Message::user("now add a regression test"),
            ],
            commits: vec![
                "1111111111111111111111111111111111111111".into(),
                "0000000000000000000000000000000000000000".into(),
            ],
            files_touched: vec!["src/auth.rs".into(), "tests/auth.rs".into()],
            redaction_count: 2,
        }
    }

    /// Minimal hand-rolled structural validation of the spec's required
    /// fields (the spec repo ships its schema as `schemas.ts`, not as
    /// standalone JSON Schema files, so the checks are inlined here).
    fn assert_spec_required_fields(v: &serde_json::Value) {
        // version: required, ^[0-9]+\.[0-9]+\.[0-9]+$
        let version = v["version"].as_str().expect("version is a string");
        let parts: Vec<&str> = version.split('.').collect();
        assert_eq!(parts.len(), 3, "version must be x.y.z: {version}");
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "version must be numeric semver: {version}"
        );

        // id: required, UUID
        let id = v["id"].as_str().expect("id is a string");
        assert_eq!(id.len(), 36, "uuid length: {id}");
        for (i, c) in id.chars().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                assert_eq!(c, '-', "uuid hyphen at {i}: {id}");
            } else {
                assert!(c.is_ascii_hexdigit(), "uuid hex at {i}: {id}");
            }
        }

        // timestamp: required, RFC 3339
        let ts = v["timestamp"].as_str().expect("timestamp is a string");
        assert_eq!(ts.len(), 20, "YYYY-MM-DDTHH:MM:SSZ: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..], "Z");

        // files: required array; each entry needs path + conversations,
        // each conversation needs ranges.
        let files = v["files"].as_array().expect("files is an array");
        for f in files {
            assert!(f["path"].is_string(), "file.path required: {f}");
            let convs = f["conversations"]
                .as_array()
                .expect("file.conversations required");
            for c in convs {
                assert!(c["ranges"].is_array(), "conversation.ranges required: {c}");
                if let Some(t) = c.get("contributor").and_then(|k| k["type"].as_str()) {
                    assert!(
                        matches!(t, "human" | "ai" | "mixed" | "unknown"),
                        "contributor.type enum: {t}"
                    );
                }
            }
        }

        // vcs, when present: type enum + revision required.
        if let Some(vcs) = v.get("vcs") {
            let t = vcs["type"].as_str().expect("vcs.type required");
            assert!(
                matches!(t, "git" | "jj" | "hg" | "svn"),
                "vcs.type enum: {t}"
            );
            assert!(vcs["revision"].is_string(), "vcs.revision required");
        }

        // tool, when present: name + version required.
        if let Some(tool) = v.get("tool") {
            assert!(tool["name"].is_string(), "tool.name required");
            assert!(tool["version"].is_string(), "tool.version required");
        }
    }

    #[test]
    fn round_trip_satisfies_spec_required_fields() {
        let s = fixture_session();
        let json = trace_json(&s).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_spec_required_fields(&v);
    }

    #[test]
    fn maps_session_identity_and_timestamp() {
        let s = fixture_session();
        let v: serde_json::Value = serde_json::from_str(&trace_json(&s).unwrap()).unwrap();
        assert_eq!(v["version"], SPEC_VERSION);
        assert_eq!(v["id"], s.id);
        assert_eq!(v["timestamp"], "2025-01-01T00:00:00Z");
        assert_eq!(v["tool"]["name"], "dkod");
        assert_eq!(v["tool"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn maps_files_with_ai_contributor_and_empty_ranges() {
        let s = fixture_session();
        let v: serde_json::Value = serde_json::from_str(&trace_json(&s).unwrap()).unwrap();
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "src/auth.rs");
        assert_eq!(files[1]["path"], "tests/auth.rs");
        for f in files {
            let convs = f["conversations"].as_array().unwrap();
            assert_eq!(convs.len(), 1);
            assert_eq!(convs[0]["contributor"]["type"], "ai");
            // dkod has file+commit granularity, not line ranges — the
            // honest export is an empty (spec-legal) ranges array.
            assert_eq!(convs[0]["ranges"], serde_json::json!([]));
            // never invent a model_id or conversation url
            assert!(convs[0].get("url").is_none());
            assert!(convs[0]["contributor"].get("model_id").is_none());
        }
    }

    #[test]
    fn maps_newest_commit_to_vcs_and_full_list_to_metadata() {
        let s = fixture_session();
        let v: serde_json::Value = serde_json::from_str(&trace_json(&s).unwrap()).unwrap();
        assert_eq!(v["vcs"]["type"], "git");
        assert_eq!(
            v["vcs"]["revision"],
            "1111111111111111111111111111111111111111"
        );
        let meta = &v["metadata"][DKOD_METADATA_KEY];
        assert_eq!(
            meta["commits"],
            serde_json::json!([
                "1111111111111111111111111111111111111111",
                "0000000000000000000000000000000000000000"
            ])
        );
    }

    #[test]
    fn session_without_commits_omits_vcs() {
        let mut s = fixture_session();
        s.commits.clear();
        let v: serde_json::Value = serde_json::from_str(&trace_json(&s).unwrap()).unwrap();
        assert!(v.get("vcs").is_none(), "vcs must be omitted, not null");
    }

    #[test]
    fn metadata_carries_agent_prompts_and_session_pointer() {
        let s = fixture_session();
        let v: serde_json::Value = serde_json::from_str(&trace_json(&s).unwrap()).unwrap();
        let meta = &v["metadata"][DKOD_METADATA_KEY];
        assert_eq!(meta["agent"], "claude_code");
        assert_eq!(meta["prompt_summary"], "fix the auth bug");
        // prompts = user messages only, in order
        assert_eq!(
            meta["prompts"],
            serde_json::json!(["fix the auth bug", "now add a regression test"])
        );
        assert_eq!(meta["session_ref"], format!("refs/dkod/sessions/{}", s.id));
        assert_eq!(meta["duration_ms"], 12_345);
        assert_eq!(meta["created_at"], 1_735_689_600);
        assert_eq!(meta["redaction_count"], 2);
    }

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_735_689_600), "2025-01-01T00:00:00Z");
        // leap day
        assert_eq!(rfc3339_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        // mid-day with minutes/seconds
        assert_eq!(rfc3339_utc(1_735_689_600 + 3_661), "2025-01-01T01:01:01Z");
        // pre-epoch stays well-formed
        assert_eq!(rfc3339_utc(-1), "1969-12-31T23:59:59Z");
    }
}
