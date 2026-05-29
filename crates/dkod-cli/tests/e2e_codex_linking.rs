//! End-to-end: `dkod capture codex` links the commit the agent made to the
//! captured session, via a fake codex binary (DKOD_CODEX_BIN).

use assert_cmd::Command;
use std::path::Path;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn head(repo: &Path) -> String {
    String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

#[test]
#[cfg(unix)]
fn capture_codex_links_agent_commit() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("f.txt"), "start\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "start"]);

    let codex_home = TempDir::new().unwrap();
    let rollout_dir = codex_home.path().join("sessions/2026/05/29");
    std::fs::create_dir_all(&rollout_dir).unwrap();

    let tid = "deadbeefcafe";
    let rollout = rollout_dir.join(format!("rollout-2026-05-29T00-00-00-{tid}.jsonl"));
    let fake = codex_home.path().join("fake-codex.sh");
    let script = format!(
        r#"#!/bin/sh
git -C "{repo}" -c user.name=t -c user.email=t@e.com commit --allow-empty -m agentwork >/dev/null 2>&1
printf '%s\n' '{{"type":"session_meta","payload":{{"cli_version":"99.0.0"}}}}' > "{rollout}"
printf '%s\n' '{{"type":"thread.started","thread_id":"{tid}"}}'
exit 0
"#,
        repo = repo.path().display(),
        rollout = rollout.display(),
        tid = tid,
    );
    std::fs::write(&fake, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(repo.path())
        .env("DKOD_CODEX_BIN", &fake)
        .env("CODEX_HOME", codex_home.path())
        .args(["capture", "codex", "--", "noop"])
        .assert()
        .success();

    let agent_commit = head(repo.path());
    let r = gix::open(repo.path()).unwrap();
    let cref = r.find_reference(&dkod_core::refs::commit_ref(&agent_commit));
    assert!(
        cref.is_ok(),
        "expected refs/dkod/commits/{agent_commit} to exist after capture"
    );
    let cref = cref.unwrap();
    let obj = r.find_object(cref.id()).unwrap().detach();
    let session: dkod_core::Session = serde_json::from_slice(&obj.data).unwrap();
    assert!(session.commits.contains(&agent_commit));
}
