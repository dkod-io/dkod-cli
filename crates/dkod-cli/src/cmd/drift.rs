//! `dkod drift` — report sessions where the agent did materially more / other
//! than the prompt asked. Pure analysis lives in `dkod_core::drift`; this
//! layer resolves sessions, derives diff stats from git, and renders.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

/// Parse `git show --numstat --format=` output (lines of `<ins>\t<del>\t<path>`,
/// where `<ins>`/`<del>` are `-` for binary files) into (files, insertions,
/// deletions). Unknown/blank lines are skipped.
fn parse_numstat(out: &str) -> (usize, usize, usize) {
    let mut files = 0usize;
    let mut ins = 0usize;
    let mut del = 0usize;
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let (a, b, path) = match (cols.next(), cols.next(), cols.next()) {
            (Some(a), Some(b), Some(p)) => (a, b, p),
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        let ai = if a == "-" {
            Some(0)
        } else {
            a.parse::<usize>().ok()
        };
        let bd = if b == "-" {
            Some(0)
        } else {
            b.parse::<usize>().ok()
        };
        let (ai, bd) = match (ai, bd) {
            (Some(ai), Some(bd)) => (ai, bd),
            _ => continue, // malformed counts → not a real numstat row
        };
        files += 1;
        ins += ai;
        del += bd;
    }
    (files, ins, del)
}

/// Sum per-commit numstat over a session's commits. `None` only when there are
/// no commits or none of them resolve (best-effort otherwise).
fn diff_stats(cwd: &Path, commits: &[String]) -> Option<dkod_core::drift::DiffStats> {
    if commits.is_empty() {
        return None;
    }
    let mut files = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut any = false;
    for sha in commits {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["show", "--numstat", "--format=", sha])
            .output();
        let out = match out {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        any = true;
        let (f, i, d) = parse_numstat(&String::from_utf8_lossy(&out.stdout));
        files += f;
        insertions += i;
        deletions += d;
    }
    if any {
        Some(dkod_core::drift::DiffStats {
            files,
            insertions,
            deletions,
        })
    } else {
        None
    }
}

fn print_detail(
    s: &dkod_core::Session,
    verdict: &dkod_core::drift::DriftVerdict,
    stats: Option<&dkod_core::drift::DiffStats>,
) {
    println!(
        "{}  {}  {}",
        s.id,
        dkod_core::agent_label(&s.agent),
        s.prompt_summary
    );
    if let Some(d) = stats {
        println!("    files: {}  +{} -{}", d.files, d.insertions, d.deletions);
    }
    if verdict.is_clean() {
        println!("    clean");
    } else {
        for r in &verdict.reasons {
            println!("    - {}", r.detail);
        }
    }
}

fn print_listing(s: &dkod_core::Session, verdict: &dkod_core::drift::DriftVerdict) {
    println!(
        "{}  {}  {}",
        s.id,
        dkod_core::agent_label(&s.agent),
        s.prompt_summary
    );
    for r in &verdict.reasons {
        println!("    - {}", r.detail);
    }
}

/// `dkod drift [session-id] [--all]`. With a session id, prints that session's
/// detail; otherwise lists sessions (flagged only, unless `all`).
/// Returns Ok for the report itself — drift findings never cause a non-zero
/// exit (this is a report, not a gate). Genuine errors (not a git repo, an
/// unknown session id, unreadable config) still propagate and exit non-zero.
pub fn run(cwd: &Path, session_id: Option<&str>, all: bool) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let cfg = super::load_config(cwd)?;

    if let Some(id) = session_id {
        let s = dkod_core::store::read_session(cwd, id)
            .map_err(|e| anyhow!("read session {id}: {e:#}"))?;
        let stats = diff_stats(cwd, &s.commits);
        let verdict = dkod_core::drift::analyze(&s, stats.as_ref(), &cfg.drift);
        print_detail(&s, &verdict, stats.as_ref());
        return Ok(());
    }

    let mut sessions: Vec<dkod_core::Session> = Vec::new();
    for id in dkod_core::store::list_sessions(cwd).map_err(|e| anyhow!("list sessions: {e:#}"))? {
        match dkod_core::store::read_session(cwd, &id) {
            Ok(s) => sessions.push(s),
            // Resilient scan (like `dkod log`), but surface the problem rather
            // than dropping it silently — a corrupt session shouldn't abort the
            // whole drift report nor vanish without a trace.
            Err(e) => eprintln!("dkod drift: skipping unreadable session {id}: {e:#}"),
        }
    }
    sessions.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });

    for s in &sessions {
        let stats = diff_stats(cwd, &s.commits);
        let verdict = dkod_core::drift::analyze(s, stats.as_ref(), &cfg.drift);
        if verdict.is_clean() && !all {
            continue;
        }
        if all && verdict.is_clean() {
            println!(
                "{}  {}  {}  [clean]",
                s.id,
                dkod_core::agent_label(&s.agent),
                s.prompt_summary
            );
        } else {
            print_listing(s, &verdict);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_numstat;
    #[test]
    fn parses_numstat_lines() {
        let out = "3\t1\tsrc/a.rs\n0\t9\tsrc/b.rs\n-\t-\tlogo.png\n\n";
        let (files, ins, del) = parse_numstat(out);
        assert_eq!(files, 3);
        assert_eq!(ins, 3);
        assert_eq!(del, 10);
    }
    #[test]
    fn ignores_malformed_lines() {
        let out = "garbage line with no tabs\n2\t2\tok.rs\n";
        assert_eq!(parse_numstat(out), (1, 2, 2));
    }
    #[test]
    fn skips_rows_with_nonnumeric_counts() {
        let out = "x\ty\tweird.rs\n2\t2\tok.rs\n";
        assert_eq!(parse_numstat(out), (1, 2, 2));
    }
}
