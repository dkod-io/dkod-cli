//! `dkod relink` — hidden, invoked by the git `post-rewrite` hook.
//!
//! Reads `old new` commit-SHA pairs from stdin (git's post-rewrite format,
//! one per rewritten commit) and re-points each session's commit-ref from the
//! old SHA to the new one, so `dkod blame` keeps resolving rewritten lines.
//! Always exits 0 — a misbehaving re-link must never break the user's rebase.

use anyhow::Result;
use std::io::Read;
use std::path::Path;

/// True iff `s` is exactly 40 lowercase hex chars (a full git SHA-1).
fn is_hex40(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parse git `post-rewrite` stdin into `(old, new)` SHA pairs. Each line is
/// whitespace-split; only lines with exactly two 40-hex tokens are kept.
/// Blank and malformed lines are skipped.
pub(crate) fn parse_pairs(input: &str) -> Vec<(String, String)> {
    input
        .lines()
        .filter_map(|line| {
            let mut toks = line.split_whitespace();
            let old = toks.next()?;
            let new = toks.next()?;
            if toks.next().is_some() {
                return None; // more than two tokens — malformed
            }
            if is_hex40(old) && is_hex40(new) {
                Some((old.to_string(), new.to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Read stdin, parse pairs, and re-link each. Best-effort: per-pair errors are
/// ignored and the command always returns `Ok(())`.
pub fn run(cwd: &Path) -> Result<()> {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf); // best-effort
    let pairs = parse_pairs(&buf);
    let mut relinked = 0usize;
    for (old, new) in &pairs {
        if let Ok(true) = dkod_core::store::relink_commit(cwd, old, new) {
            relinked += 1;
        }
    }
    if relinked > 0 {
        eprintln!("dkod: re-linked {relinked} session commit-ref(s) after history rewrite");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0000000000000000000000000000000000000001";
    const B: &str = "0000000000000000000000000000000000000002";
    const C: &str = "0000000000000000000000000000000000000003";

    #[test]
    fn parses_well_formed_pairs() {
        let input = format!("{A} {B}\n{B} {C}\n");
        assert_eq!(
            parse_pairs(&input),
            vec![
                (A.to_string(), B.to_string()),
                (B.to_string(), C.to_string())
            ]
        );
    }

    #[test]
    fn skips_blank_and_single_token_lines() {
        let input = format!("\n{A}\n   \n{A} {B}\n");
        assert_eq!(parse_pairs(&input), vec![(A.to_string(), B.to_string())]);
    }

    #[test]
    fn rejects_non_hex_and_wrong_length() {
        let input = format!("{A} zzzz\nshort {B}\n{A} {B}extra\n");
        assert!(parse_pairs(&input).is_empty());
    }

    #[test]
    fn rejects_three_token_lines() {
        let input = format!("{A} {B} {C}\n");
        assert!(parse_pairs(&input).is_empty());
    }

    #[test]
    fn preserves_order_for_many_to_one() {
        let input = format!("{A} {C}\n{B} {C}\n");
        assert_eq!(
            parse_pairs(&input),
            vec![
                (A.to_string(), C.to_string()),
                (B.to_string(), C.to_string())
            ]
        );
    }

    #[test]
    fn is_hex40_validates() {
        assert!(is_hex40(A));
        assert!(!is_hex40("ABCDEF0000000000000000000000000000000001")); // uppercase
        assert!(!is_hex40("000")); // too short
        assert!(!is_hex40("g000000000000000000000000000000000000000")); // non-hex
    }
}
