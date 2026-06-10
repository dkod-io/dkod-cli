//! `dkod import` — transcript-rescue importers.
//!
//! Agent CLIs keep their own local session logs (Claude Code transcripts
//! expire after ~30 days; Codex rollouts linger but live outside the repo).
//! `dkod import <source>` parses those logs into [`dkod_core::Session`]
//! blobs under `refs/dkod/sessions/<id>` in the current repo, applying the
//! same redaction pass as live capture. It needs only a git repo — no
//! `dkod init` / hooks required.
//!
//! Id / dedup semantics:
//!
//! - The imported session id is the SOURCE TOOL's own session UUID — the
//!   Claude Code transcript filename stem, or the Codex `session_meta`
//!   thread id (falling back to the rollout filename suffix). That makes
//!   re-imports idempotent: a session whose ref already exists is skipped.
//! - Live capture mints a fresh UUIDv7 per flush (`parse_transcript` /
//!   `parse_rollout` call `Session::new_id`), so a conversation captured
//!   live AND later imported is stored twice under different ids. The two
//!   id namespaces never collide, and there is no cheap way to tell a
//!   live-captured conversation apart from its on-disk transcript, so V1
//!   accepts the potential duplicate.
//!
//! Source roots:
//!
//! - Claude Code: `~/.claude/projects/<mapped-cwd>/*.jsonl`, where the
//!   mapping replaces every non-alphanumeric char of the canonicalized
//!   repo path with `-`. `--project <dir>` overrides the source directory
//!   outright. The env var `DKOD_CLAUDE_DIR` (test override, also handy
//!   for nonstandard installs) replaces the `~/.claude` root.
//! - Codex: `$CODEX_HOME/sessions` (default `~/.codex/sessions`), scanned
//!   recursively. `--dir <dir>` overrides it. Rollouts whose recorded
//!   `cwd` lies outside the current repo belong to other projects and are
//!   ignored (not counted as skipped).
//!
//! Resilience: unparseable files warn on stderr, count as skipped, and
//! never abort the scan.

use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Tally of one import run. `skipped` counts already-captured ids and
/// unparseable files; out-of-scope files (e.g. Codex rollouts recorded in
/// another project) are not counted at all.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub imported: usize,
    pub skipped: usize,
}

fn print_summary(o: &Outcome) {
    println!(
        "imported {} session(s), skipped {} (already captured / unparseable)",
        o.imported, o.skipped
    );
}

/// `dkod import claude-code [--project <dir>]`.
pub fn run_claude_code(cwd: &Path, project: Option<&Path>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let cfg = super::load_config(cwd)?;
    let source = match project {
        Some(p) => p.to_path_buf(),
        None => default_claude_source_dir(cwd)?,
    };
    let outcome = import_claude_dir(cwd, &source, &cfg);
    print_summary(&outcome);
    Ok(())
}

/// `dkod import codex [--dir <dir>]`.
pub fn run_codex(cwd: &Path, dir: Option<&Path>) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let cfg = super::load_config(cwd)?;
    let source = match dir {
        Some(p) => p.to_path_buf(),
        None => default_codex_sessions_dir()?,
    };
    let repo_scope = canonical_or_self(cwd);
    let outcome = import_codex_dir(cwd, &source, &cfg, &repo_scope);
    print_summary(&outcome);
    Ok(())
}

// --- source-dir resolution -------------------------------------------------

/// `<claude root>/projects/<mapped repo path>`. The root is `~/.claude`
/// unless `DKOD_CLAUDE_DIR` overrides it (documented test/nonstandard-install
/// hook). The repo path is canonicalized first because Claude Code records
/// the physical cwd (macOS tempdir symlinks, etc.).
fn default_claude_source_dir(cwd: &Path) -> Result<PathBuf> {
    let root = match std::env::var_os("DKOD_CLAUDE_DIR") {
        Some(d) => PathBuf::from(d),
        None => dirs::home_dir()
            .context("cannot resolve home directory for ~/.claude")?
            .join(".claude"),
    };
    Ok(root
        .join("projects")
        .join(claude_project_dir_name(&canonical_or_self(cwd))))
}

/// `$CODEX_HOME/sessions`, default `~/.codex/sessions` — same resolution
/// the live Codex capture wrapper uses.
fn default_codex_sessions_dir() -> Result<PathBuf> {
    let home: PathBuf = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .ok_or_else(|| anyhow!("could not resolve CODEX_HOME or $HOME/.codex"))?;
    Ok(home.join("sessions"))
}

fn canonical_or_self(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Claude Code's on-disk project-dir encoding: every non-alphanumeric char
/// of the absolute cwd becomes `-` (so `/Users/x/my.repo` ->
/// `-Users-x-my-repo`). Observed against real `~/.claude/projects` entries.
fn claude_project_dir_name(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

// --- shared import plumbing -------------------------------------------------

/// Ids already stored under `refs/dkod/sessions/*`. A listing failure is
/// treated as "none known" — the importer must work in a repo that has
/// never seen dkod before.
fn existing_session_ids(repo: &Path) -> HashSet<String> {
    dkod_core::store::list_sessions(repo)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// `36` chars shaped `8-4-4-4-12` hex — both Claude Code transcript stems
/// and Codex thread ids. Doubles as a ref-name safety gate, since the id is
/// interpolated into `refs/dkod/sessions/<id>`.
fn is_uuid_like(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    s.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// If the parser found no usable entry timestamps (`created_at == 0`),
/// fall back to the transcript file's mtime.
fn fill_created_at_from_mtime(session: &mut dkod_core::Session, path: &Path) {
    if session.created_at != 0 {
        return;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(modified) = meta.modified() {
            if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                session.created_at = i64::try_from(d.as_secs()).unwrap_or(0);
            }
        }
    }
}

/// Redact and write one parsed session under `id`. Returns `true` on
/// success; failures warn and return `false` so the scan continues.
fn store_imported(
    repo: &Path,
    cfg: &dkod_core::config::Config,
    mut session: dkod_core::Session,
    id: &str,
    src: &Path,
) -> bool {
    session.id = id.to_string();
    fill_created_at_from_mtime(&mut session, src);
    // Imports MUST be redacted exactly like live captures.
    dkod_core::redact::redact_session(&mut session, &cfg.redact);
    match dkod_core::store::write_session(repo, &session) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("dkod: import: failed to write {}: {e:#}", src.display());
            false
        }
    }
}

// --- Claude Code -------------------------------------------------------------

/// Scan `dir` for `<uuid>.jsonl` transcripts and import each one. Missing
/// or unreadable `dir` is a clean no-op (zero counts) — there is simply
/// nothing to rescue.
fn import_claude_dir(repo: &Path, dir: &Path, cfg: &dkod_core::config::Config) -> Outcome {
    let mut out = Outcome::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            eprintln!(
                "dkod: import: no Claude Code transcripts found at {}",
                dir.display()
            );
            return out;
        }
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    files.sort();

    let existing = existing_session_ids(repo);
    for path in files {
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) if is_uuid_like(stem) => stem.to_string(),
            _ => {
                eprintln!(
                    "dkod: import: skipping {} (filename is not a session UUID)",
                    path.display()
                );
                out.skipped += 1;
                continue;
            }
        };
        if existing.contains(&id) {
            out.skipped += 1;
            continue;
        }
        let session = match dkod_core::capture::claude_code::parse_transcript(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("dkod: import: skipping {}: {e:#}", path.display());
                out.skipped += 1;
                continue;
            }
        };
        if store_imported(repo, cfg, session, &id, &path) {
            out.imported += 1;
        } else {
            out.skipped += 1;
        }
    }
    out
}

// --- Codex --------------------------------------------------------------------

/// Thread id + recorded cwd pulled from a rollout's `session_meta` line.
#[derive(Debug, Default)]
struct RolloutMeta {
    id: Option<String>,
    cwd: Option<PathBuf>,
}

/// How many leading lines to scan for `session_meta`. It is the first line
/// in every rollout Codex writes; the margin tolerates future preamble.
const ROLLOUT_META_SCAN_LINES: usize = 20;

fn rollout_meta(path: &Path) -> RolloutMeta {
    use std::io::BufRead;
    let mut meta = RolloutMeta::default();
    let Ok(file) = std::fs::File::open(path) else {
        return meta;
    };
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(ROLLOUT_META_SCAN_LINES)
    {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
            continue;
        }
        let payload = v.get("payload").cloned().unwrap_or_default();
        meta.id = payload
            .get("id")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        meta.cwd = payload
            .get("cwd")
            .and_then(|x| x.as_str())
            .map(PathBuf::from);
        break;
    }
    meta
}

/// Fallback thread-id source: `rollout-<timestamp>-<uuid>.jsonl`.
fn rollout_filename_thread_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() < 36 {
        return None;
    }
    let tail = &stem[stem.len() - 36..];
    is_uuid_like(tail).then(|| tail.to_string())
}

fn collect_jsonl_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_recursive(&path, out);
        } else if path.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Recursively scan `dir` (Codex lays rollouts out as
/// `sessions/YYYY/MM/DD/rollout-*.jsonl`) and import every rollout whose
/// recorded cwd is inside `repo_scope`. Rollouts from other projects are
/// ignored silently except for a summary note; rollouts with no recorded
/// cwd are imported (rescue bias).
fn import_codex_dir(
    repo: &Path,
    dir: &Path,
    cfg: &dkod_core::config::Config,
    repo_scope: &Path,
) -> Outcome {
    let mut out = Outcome::default();
    let mut files = Vec::new();
    collect_jsonl_recursive(dir, &mut files);
    if files.is_empty() {
        eprintln!("dkod: import: no Codex rollouts found at {}", dir.display());
        return out;
    }
    files.sort();

    let existing = existing_session_ids(repo);
    let mut other_project = 0usize;
    for path in files {
        let meta = rollout_meta(&path);
        let id = meta
            .id
            .filter(|i| is_uuid_like(i))
            .or_else(|| rollout_filename_thread_id(&path));
        let Some(id) = id else {
            eprintln!(
                "dkod: import: skipping {} (no thread id in session_meta or filename)",
                path.display()
            );
            out.skipped += 1;
            continue;
        };
        if let Some(cwd) = &meta.cwd {
            if !cwd.starts_with(repo_scope) {
                other_project += 1;
                continue;
            }
        }
        if existing.contains(&id) {
            out.skipped += 1;
            continue;
        }
        let session = match dkod_core::capture::codex::parse_rollout(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("dkod: import: skipping {}: {e:#}", path.display());
                out.skipped += 1;
                continue;
            }
        };
        if store_imported(repo, cfg, session, &id, &path) {
            out.imported += 1;
        } else {
            out.skipped += 1;
        }
    }
    if other_project > 0 {
        eprintln!("dkod: import: ignored {other_project} rollout(s) recorded outside this repo");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        gix::init(tmp.path()).unwrap();
        tmp
    }

    fn cfg() -> dkod_core::config::Config {
        dkod_core::config::Config::default()
    }

    const CLAUDE_ID: &str = "11111111-2222-4333-8444-555555555555";
    const CODEX_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    fn claude_fixture() -> String {
        let aws = "AKIAIOSFODNN7EXAMPLE"; // gitleaks:allow
        format!(
            concat!(
                r#"{{"type":"user","timestamp":"2026-05-03T12:00:01.000Z","message":{{"content":"fix the login bug {aws}"}}}}"#,
                "\n",
                r#"{{"type":"assistant","timestamp":"2026-05-03T12:00:03.000Z","message":{{"content":[{{"type":"text","text":"done"}}]}}}}"#,
                "\n",
            ),
            aws = aws
        )
    }

    fn codex_fixture(thread_id: &str, cwd: &str) -> String {
        format!(
            concat!(
                r#"{{"timestamp":"2026-05-03T12:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}","originator":"codex_cli_rs","cli_version":"0.34.0"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-03T12:00:02.000Z","type":"event_msg","payload":{{"type":"user_message","message":"add hello.txt"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-03T12:00:05.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"created hello.txt"}}]}}}}"#,
                "\n",
            ),
            id = thread_id,
            cwd = cwd
        )
    }

    // --- mapping / id helpers ------------------------------------------

    #[test]
    fn claude_project_dir_name_maps_every_non_alphanumeric_to_dash() {
        assert_eq!(
            claude_project_dir_name(Path::new("/Users/foo/my.repo_x")),
            "-Users-foo-my-repo-x"
        );
        // pre-existing dashes survive (ambiguity is upstream's, not ours)
        assert_eq!(
            claude_project_dir_name(Path::new("/Users/haim-ari/github/dkod-cli")),
            "-Users-haim-ari-github-dkod-cli"
        );
    }

    #[test]
    fn is_uuid_like_accepts_uuids_and_rejects_garbage() {
        assert!(is_uuid_like(CLAUDE_ID));
        assert!(is_uuid_like("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE"));
        assert!(!is_uuid_like("not-a-session"));
        assert!(!is_uuid_like("11111111222243338444555555555555")); // no dashes
        assert!(!is_uuid_like("../../../../etc/passwd/oops-oops-oop"));
    }

    #[test]
    fn rollout_filename_thread_id_extracts_trailing_uuid() {
        let p = Path::new(
            "/x/2026/05/03/rollout-2026-05-03T12-00-00-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee.jsonl",
        );
        assert_eq!(rollout_filename_thread_id(p).as_deref(), Some(CODEX_ID));
        assert_eq!(
            rollout_filename_thread_id(Path::new("/x/short.jsonl")),
            None
        );
    }

    // --- Claude Code transcript -> Session mapping ----------------------

    #[test]
    fn import_claude_dir_maps_transcript_to_session_with_redaction() {
        let repo = init_repo();
        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join(format!("{CLAUDE_ID}.jsonl")),
            claude_fixture(),
        )
        .unwrap();

        let out = import_claude_dir(repo.path(), src.path(), &cfg());
        assert_eq!(
            out,
            Outcome {
                imported: 1,
                skipped: 0
            }
        );

        let s = dkod_core::store::read_session(repo.path(), CLAUDE_ID).unwrap();
        assert!(matches!(s.agent, dkod_core::Agent::ClaudeCode));
        assert_eq!(s.id, CLAUDE_ID, "id is the transcript filename UUID");
        assert!(s.prompt_summary.starts_with("fix the login bug"));
        assert_eq!(s.created_at, 1777809601, "created_at from entry timestamps");
        assert!(s.commits.is_empty(), "imports cannot attribute commits");
        // redacted like a live capture
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(json.contains("[REDACTED"));
        assert!(s.redaction_count >= 1);
    }

    #[test]
    fn import_claude_dir_is_idempotent_and_skips_non_uuid_files() {
        let repo = init_repo();
        let src = tempfile::TempDir::new().unwrap();
        std::fs::write(
            src.path().join(format!("{CLAUDE_ID}.jsonl")),
            claude_fixture(),
        )
        .unwrap();
        std::fs::write(src.path().join("junk.jsonl"), "{}\n").unwrap();
        std::fs::write(src.path().join("README.txt"), "ignored entirely").unwrap();

        let first = import_claude_dir(repo.path(), src.path(), &cfg());
        assert_eq!(
            first,
            Outcome {
                imported: 1,
                skipped: 1
            }
        );
        let second = import_claude_dir(repo.path(), src.path(), &cfg());
        assert_eq!(
            second,
            Outcome {
                imported: 0,
                skipped: 2
            },
            "re-import must skip the already-captured id"
        );
    }

    #[test]
    fn import_claude_dir_missing_dir_is_a_clean_noop() {
        let repo = init_repo();
        let out = import_claude_dir(repo.path(), Path::new("/nonexistent/nowhere"), &cfg());
        assert_eq!(out, Outcome::default());
    }

    // --- Codex rollout -> Session mapping -------------------------------

    #[test]
    fn import_codex_dir_maps_rollout_and_filters_other_projects() {
        let repo = init_repo();
        let scope = canonical_or_self(repo.path());
        let src = tempfile::TempDir::new().unwrap();
        let day = src.path().join("2026/05/03");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-05-03T12-00-00-{CODEX_ID}.jsonl")),
            codex_fixture(CODEX_ID, &scope.display().to_string()),
        )
        .unwrap();
        let other = "99999999-8888-4777-8666-555555555555";
        std::fs::write(
            day.join(format!("rollout-2026-05-03T13-00-00-{other}.jsonl")),
            codex_fixture(other, "/somewhere/else"),
        )
        .unwrap();

        let out = import_codex_dir(repo.path(), src.path(), &cfg(), &scope);
        assert_eq!(
            out,
            Outcome {
                imported: 1,
                skipped: 0
            },
            "other-project rollout must be ignored, not skipped"
        );

        let s = dkod_core::store::read_session(repo.path(), CODEX_ID).unwrap();
        assert!(matches!(s.agent, dkod_core::Agent::Codex));
        assert_eq!(s.id, CODEX_ID, "id is the session_meta thread id");
        assert!(s.prompt_summary.starts_with("add hello.txt"));
        assert_eq!(s.created_at, 1777809602);
        assert!(dkod_core::store::read_session(repo.path(), other).is_err());

        // idempotent re-import
        let again = import_codex_dir(repo.path(), src.path(), &cfg(), &scope);
        assert_eq!(
            again,
            Outcome {
                imported: 0,
                skipped: 1
            }
        );
    }

    #[test]
    fn import_codex_dir_falls_back_to_filename_thread_id() {
        let repo = init_repo();
        let scope = canonical_or_self(repo.path());
        let src = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(src.path()).unwrap();
        // session_meta line missing entirely -> id comes from the filename;
        // no recorded cwd -> imported (rescue bias).
        let body = concat!(
            r#"{"timestamp":"2026-05-03T12:00:02.000Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            "\n"
        );
        std::fs::write(
            src.path()
                .join(format!("rollout-2026-05-03T12-00-00-{CODEX_ID}.jsonl")),
            body,
        )
        .unwrap();

        let out = import_codex_dir(repo.path(), src.path(), &cfg(), &scope);
        assert_eq!(
            out,
            Outcome {
                imported: 1,
                skipped: 0
            }
        );
        assert!(dkod_core::store::read_session(repo.path(), CODEX_ID).is_ok());
    }

    #[test]
    fn fill_created_at_falls_back_to_file_mtime() {
        let src = tempfile::TempDir::new().unwrap();
        let path = src.path().join("t.jsonl");
        std::fs::write(&path, "x").unwrap();
        let mut s = dkod_core::Session {
            id: "x".into(),
            agent: dkod_core::Agent::ClaudeCode,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: String::new(),
            messages: vec![],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        };
        fill_created_at_from_mtime(&mut s, &path);
        assert!(
            s.created_at > 0,
            "mtime fallback should populate created_at"
        );
        // and a parser-provided value is never overwritten
        let mut s2 = s.clone();
        s2.created_at = 42;
        fill_created_at_from_mtime(&mut s2, &path);
        assert_eq!(s2.created_at, 42);
    }
}
