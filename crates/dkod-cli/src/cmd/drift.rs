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

/// Total card width in visible columns, borders included.
const CARD_WIDTH: usize = 64;
/// Content columns between the `│ ` and ` │` gutters.
const CARD_INNER: usize = CARD_WIDTH - 4;

/// Truncate `s` to at most `max` visible chars, ending with `…` when cut.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// One boxed card row: `│ <prefix><rest><padding> │`. The prefix is rendered
/// with the given SGR code (e.g. `"33"` = yellow) when `color` is on; `rest`
/// is truncated to fit so every row is exactly `CARD_WIDTH` visible columns.
fn card_row(prefix: &str, prefix_sgr: Option<&str>, rest: &str, color: bool) -> String {
    let prefix_vis = truncate_ellipsis(prefix, CARD_INNER);
    let rest_max = CARD_INNER - prefix_vis.chars().count();
    let rest_vis = truncate_ellipsis(rest, rest_max);
    let pad = CARD_INNER - prefix_vis.chars().count() - rest_vis.chars().count();
    let prefix_out = match (color, prefix_sgr) {
        (true, Some(sgr)) if !prefix_vis.is_empty() => {
            format!("\u{1b}[{sgr}m{prefix_vis}\u{1b}[0m")
        }
        _ => prefix_vis,
    };
    format!("│ {prefix_out}{rest_vis}{} │", " ".repeat(pad))
}

/// Stable human badge for a drift rule tag.
fn rule_badge(rule: &str) -> String {
    match rule {
        "sensitive_path" => "⚠ sensitive-path".into(),
        "magnitude" => "⚠ magnitude".into(),
        "unmentioned" => "⚠ unmentioned".into(),
        other => format!("⚠ {}", other.replace('_', "-")),
    }
}

/// Render the shareable drift card for one session. Pure — no I/O, no tty
/// probing; the caller decides `color` (ANSI only when stdout is a tty).
/// Every line is exactly `CARD_WIDTH` visible columns.
pub fn render_card(
    s: &dkod_core::Session,
    verdict: &dkod_core::drift::DriftVerdict,
    stats: Option<&dkod_core::drift::DiffStats>,
    color: bool,
) -> String {
    let mut out = String::new();
    let horiz = "─".repeat(CARD_WIDTH - 2);
    out.push_str(&format!("╭{horiz}╮\n"));

    // Header: command left, short session id + agent right-aligned.
    let short_id: String = s.id.chars().take(8).collect();
    let left = "dkod drift";
    let right = format!("{short_id} · {}", dkod_core::agent_label(&s.agent));
    let gap = CARD_INNER.saturating_sub(left.chars().count() + right.chars().count());
    out.push_str(&card_row(
        left,
        None,
        &format!("{}{right}", " ".repeat(gap)),
        color,
    ));
    out.push('\n');

    let blank = card_row("", None, "", color);
    out.push_str(&blank);
    out.push('\n');

    out.push_str(&card_row(
        "ASKED: ",
        None,
        &format!("\"{}\"", s.prompt_summary),
        color,
    ));
    out.push('\n');
    out.push_str(&blank);
    out.push('\n');

    if verdict.is_clean() {
        out.push_str(&card_row(
            "✓",
            Some("32"),
            " in scope — output matches the ask",
            color,
        ));
        out.push('\n');
    } else {
        out.push_str(&card_row("DID:", None, "", color));
        out.push('\n');
        for r in &verdict.reasons {
            out.push_str(&card_row(
                &format!("  {}", rule_badge(r.rule)),
                Some("33"),
                &format!("  {}", r.detail),
                color,
            ));
            out.push('\n');
        }
        if let Some(d) = stats {
            out.push_str(&blank);
            out.push('\n');
            out.push_str(&card_row(
                "",
                None,
                &format!("files: {}   +{}/-{}", d.files, d.insertions, d.deletions),
                color,
            ));
            out.push('\n');
        }
    }

    out.push_str(&blank);
    out.push('\n');
    out.push_str(&card_row(
        "dkod — the git-native flight recorder for AI agents",
        Some("2"),
        "",
        color,
    ));
    out.push('\n');
    out.push_str(&format!("╰{horiz}╯\n"));
    out
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

/// `dkod drift [session-id] [--all] [--card]`. With a session id, prints that
/// session's detail (or, with `card`, a shareable boxed card); otherwise lists
/// sessions (flagged only, unless `all`).
/// Returns Ok for the report itself — drift findings never cause a non-zero
/// exit (this is a report, not a gate). Genuine errors (not a git repo, an
/// unknown session id, unreadable config) still propagate and exit non-zero.
pub fn run(cwd: &Path, session_id: Option<&str>, all: bool, card: bool) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;
    let cfg = super::load_config(cwd)?;

    if let Some(id) = session_id {
        let s = dkod_core::store::read_session(cwd, id)
            .map_err(|e| anyhow!("read session {id}: {e:#}"))?;
        let stats = diff_stats(cwd, &s.commits);
        let verdict = dkod_core::drift::analyze(&s, stats.as_ref(), &cfg.drift);
        if card {
            use std::io::IsTerminal;
            let color = std::io::stdout().is_terminal();
            print!("{}", render_card(&s, &verdict, stats.as_ref(), color));
        } else {
            print_detail(&s, &verdict, stats.as_ref());
        }
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
mod card_tests {
    use super::render_card;
    use dkod_core::drift::{DiffStats, DriftReason, DriftVerdict};
    use dkod_core::{Agent, Message, Session};

    fn session(prompt: &str) -> Session {
        Session {
            id: "0192f8e2-7b3a-7000-8a3e-000000000001".into(),
            agent: Agent::ClaudeCode,
            created_at: 0,
            duration_ms: 0,
            prompt_summary: prompt.into(),
            messages: vec![Message::user(prompt)],
            commits: vec![],
            files_touched: vec![],
            redaction_count: 0,
        }
    }

    fn flagged_verdict() -> DriftVerdict {
        DriftVerdict {
            reasons: vec![
                DriftReason {
                    rule: "sensitive_path",
                    detail: "touched sensitive path: .github/workflows/ci.yml".into(),
                },
                DriftReason {
                    rule: "magnitude",
                    detail: "small-sounding request but 6 files / 412 lines changed".into(),
                },
                DriftReason {
                    rule: "unmentioned",
                    detail: "prompt referenced auth.rs; also changed unrelated billing.rs".into(),
                },
            ],
        }
    }

    /// Strip ANSI SGR escape sequences (`ESC [ ... m`).
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn flagged_card_has_box_header_reasons_stats_and_footer() {
        let s = session("fix the typo in the readme");
        let stats = DiffStats {
            files: 6,
            insertions: 400,
            deletions: 12,
        };
        let card = render_card(&s, &flagged_verdict(), Some(&stats), false);
        for corner in ["╭", "╮", "╰", "╯"] {
            assert!(card.contains(corner), "missing {corner}:\n{card}");
        }
        assert!(card.contains("dkod drift"), "missing header:\n{card}");
        assert!(card.contains("0192f8e2"), "missing short id:\n{card}");
        assert!(card.contains("claude_code"), "missing agent:\n{card}");
        assert!(
            card.contains("ASKED: \"fix the typo in the readme\""),
            "missing ASKED line:\n{card}"
        );
        assert!(card.contains("DID:"), "missing DID section:\n{card}");
        for badge in ["⚠ sensitive-path", "⚠ magnitude", "⚠ unmentioned"] {
            assert!(card.contains(badge), "missing badge {badge}:\n{card}");
        }
        assert!(
            card.contains("files: 6   +400/-12"),
            "missing stats line:\n{card}"
        );
        assert!(
            card.contains("dkod — the git-native flight recorder for AI agents"),
            "missing footer:\n{card}"
        );
    }

    #[test]
    fn clean_card_has_in_scope_line() {
        let s = session("add request logging to src/server.js");
        let card = render_card(&s, &DriftVerdict::default(), None, false);
        assert!(
            card.contains("✓ in scope — output matches the ask"),
            "missing clean line:\n{card}"
        );
        assert!(
            !card.contains("DID:"),
            "clean card must not have DID:\n{card}"
        );
    }

    #[test]
    fn every_line_is_exactly_64_visible_cols() {
        let s = session("fix the typo in the readme");
        let stats = DiffStats {
            files: 6,
            insertions: 400,
            deletions: 12,
        };
        for color in [false, true] {
            for card in [
                render_card(&s, &flagged_verdict(), Some(&stats), color),
                render_card(&s, &DriftVerdict::default(), None, color),
            ] {
                for line in card.lines() {
                    let visible = strip_ansi(line);
                    assert_eq!(
                        visible.chars().count(),
                        64,
                        "line not 64 cols (color={color}): {visible:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn long_prompt_is_truncated_with_ellipsis_and_no_overflow() {
        let long: String = "x".repeat(300);
        let s = session(&long);
        let card = render_card(&s, &DriftVerdict::default(), None, false);
        assert!(card.contains('…'), "expected ellipsis:\n{card}");
        for line in card.lines() {
            assert_eq!(line.chars().count(), 64, "overflowing line: {line:?}");
        }
    }

    #[test]
    fn color_only_when_requested() {
        let s = session("fix the typo in the readme");
        let plain = render_card(&s, &flagged_verdict(), None, false);
        assert!(!plain.contains('\u{1b}'), "plain card must have no ANSI");
        let colored = render_card(&s, &flagged_verdict(), None, true);
        assert!(colored.contains('\u{1b}'), "colored card must have ANSI");
    }
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
