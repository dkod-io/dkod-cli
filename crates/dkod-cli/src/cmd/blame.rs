// Used by `dkod blame` (Task 2.2, same module). The parser lands first under TDD;
// `#[allow(dead_code)]` keeps the `-D warnings` gate green until 2.2 wires it in.
#[allow(dead_code)]
pub(crate) struct BlameLine {
    pub sha: String,
    pub content: String,
}

/// Parse `git blame --porcelain` output into one BlameLine per source line.
/// In porcelain format, a header line begins with a 40-hex commit SHA followed
/// by line numbers; metadata lines follow; the actual source text is the line
/// prefixed with a TAB. The current SHA is the most recent header seen.
#[allow(dead_code)]
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
