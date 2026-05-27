use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

pub(crate) struct BlameLine {
    pub sha: String,
    pub content: String,
}

/// Annotate each line of `path` with the AI agent session that produced its
/// commit. Lines whose commit has a `refs/dkod/commits/<sha>` link show the
/// agent, short session id, and prompt summary; all others show a short sha and
/// a `(human)` marker.
pub fn run(cwd: &Path, path: &str) -> Result<()> {
    gix::open(cwd).map_err(|_| anyhow!("not a git repo (run `git init` first)"))?;

    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["blame", "--porcelain", "--", path])
        .output()
        .context("run git blame")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git blame failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines = parse_porcelain(&text);

    let mut cache: std::collections::HashMap<String, Option<(String, String, String)>> =
        std::collections::HashMap::new();

    for (idx, bl) in lines.iter().enumerate() {
        let lineno = idx + 1;
        let annotation = cache
            .entry(bl.sha.clone())
            .or_insert_with(|| session_for_commit(cwd, &bl.sha));
        match annotation {
            Some((agent, short, summary)) => {
                println!("{lineno:>5} {agent:<11} {short} {summary} | {}", bl.content);
            }
            None => {
                if bl.sha.bytes().all(|b| b == b'0') {
                    // git blame emits an all-zeros sha for not-yet-committed /
                    // working-tree-modified lines — these aren't a human commit.
                    println!(
                        "{lineno:>5} {:<11}          | {}",
                        "(uncommitted)", bl.content
                    );
                } else {
                    let short = &bl.sha[..bl.sha.len().min(8)];
                    println!("{lineno:>5} {:<11} {short} | {}", "(human)", bl.content);
                }
            }
        }
    }
    Ok(())
}

/// If `sha` has a `refs/dkod/commits/<sha>` link, resolve the session and return
/// (agent_label, short_session_id, prompt_summary). Otherwise None.
fn session_for_commit(cwd: &Path, sha: &str) -> Option<(String, String, String)> {
    let repo = gix::open(cwd).ok()?;
    let r = repo
        .find_reference(&dkod_core::refs::commit_ref(sha))
        .ok()?;
    let obj = repo.find_object(r.id()).ok()?.detach();
    let session: dkod_core::Session = serde_json::from_slice(&obj.data).ok()?;
    let short = session.id.get(..8).unwrap_or(&session.id).to_string();
    Some((
        dkod_core::agent_label(&session.agent).to_string(),
        short,
        session.prompt_summary,
    ))
}

/// Parse `git blame --porcelain` output into one BlameLine per source line.
/// In porcelain format, a header line begins with a 40-hex commit SHA followed
/// by line numbers; metadata lines follow; the actual source text is the line
/// prefixed with a TAB. The current SHA is the most recent header seen.
pub(crate) fn parse_porcelain(out: &str) -> Vec<BlameLine> {
    let mut cur_sha: Option<String> = None;
    let mut lines = Vec::new();
    for raw in out.lines() {
        if let Some(rest) = raw.strip_prefix('\t') {
            if let Some(sha) = &cur_sha {
                lines.push(BlameLine {
                    sha: sha.clone(),
                    content: rest.to_string(),
                });
            }
        } else if let Some((maybe_sha, _)) = raw.split_once(' ') {
            if maybe_sha.len() == 40 && maybe_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                cur_sha = Some(maybe_sha.to_string());
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal `git blame --porcelain` sample: two lines from two commits.
    const SAMPLE: &str = "\
0000000000000000000000000000000000000001 1 1 1
author Alice
\tfn main() {
0000000000000000000000000000000000000002 2 2 1
author Bob
\tprintln!(\"hi\");
";

    #[test]
    fn parses_porcelain_into_lines() {
        let lines = parse_porcelain(SAMPLE);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].sha, "0000000000000000000000000000000000000001");
        assert_eq!(lines[0].content, "fn main() {");
        assert_eq!(lines[1].sha, "0000000000000000000000000000000000000002");
        assert_eq!(lines[1].content, "println!(\"hi\");");
    }
}
