pub fn session_ref(id: &str) -> String {
    format!("refs/dkod/sessions/{id}")
}

pub fn commit_ref(sha: &str) -> String {
    format!("refs/dkod/commits/{sha}")
}

pub fn parse_session_ref(r: &str) -> Option<String> {
    r.strip_prefix("refs/dkod/sessions/").map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ref_path_is_correct() {
        let id = "0192f8e2-7b3a-7000-8a3e-000000000001";
        assert_eq!(
            session_ref(id),
            "refs/dkod/sessions/0192f8e2-7b3a-7000-8a3e-000000000001"
        );
    }

    #[test]
    fn commit_ref_path_is_correct() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(
            commit_ref(sha),
            "refs/dkod/commits/deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        );
    }

    #[test]
    fn parses_session_ref() {
        let r = "refs/dkod/sessions/abc-123";
        assert_eq!(parse_session_ref(r), Some("abc-123".to_string()));
    }

    #[test]
    fn rejects_non_session_ref() {
        assert_eq!(parse_session_ref("refs/heads/main"), None);
    }
}
