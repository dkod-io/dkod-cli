//! End-to-end smoke test for the dkod V1 lifecycle.
//!
//! Proves session refs at `refs/dkod/sessions/*` round-trip through plain
//! `git push` / `git clone` / `git fetch`. This is the load-bearing claim of
//! the whole product: dkod data rides on normal Git transport and shows up in
//! a fresh clone after an explicit fetch of the dkod ref namespace.
//!
//! TODO(v1.5): `git clone` does not pull non-default refs, so users currently
//! need an explicit `git fetch origin '+refs/dkod/*:refs/dkod/*'` after
//! cloning. The right polish is to have `dkod init` write a
//! `[remote "origin"] fetch = +refs/dkod/*:refs/dkod/*` line into the local
//! `.git/config` so the dkod refs come down on every routine fetch. Out of
//! scope for this task; tracked separately.

use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;
use std::process::Command as Std;

fn git(cwd: &Path, args: &[&str]) -> Std {
    let mut c = Std::new("git");
    c.current_dir(cwd);
    for a in args {
        c.arg(a);
    }
    c
}

fn must(mut cmd: Std, label: &str) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{label}: spawn: {e}"));
    if !out.status.success() {
        panic!(
            "{label}: status {:?}\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

#[test]
fn full_lifecycle_through_git_push_and_fetch() {
    let scratch = tempfile::TempDir::new().unwrap();
    let bare = scratch.path().join("origin.git");
    let work = scratch.path().join("work");
    let other = scratch.path().join("other");

    // 1. Init bare remote.
    let mut init_bare = Std::new("git");
    init_bare.args(["init", "--bare", bare.to_str().unwrap()]);
    must(init_bare, "git init bare");

    // 2. Init working clone (init + add remote).
    std::fs::create_dir_all(&work).unwrap();
    must(git(&work, &["init"]), "git init work");
    must(
        git(&work, &["config", "user.name", "smoketest"]),
        "git config name",
    );
    must(
        git(&work, &["config", "user.email", "smoke@example.com"]),
        "git config email",
    );
    must(
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]),
        "git remote add",
    );

    // 3. dkod init in working clone.
    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&work)
        .arg("init")
        .assert()
        .success();
    assert!(work.join(".dkod/config.toml").exists());

    // 4. Make a real commit on main.
    std::fs::write(work.join("README.md"), "# smoke test\n").unwrap();
    must(git(&work, &["add", "README.md"]), "git add");
    must(git(&work, &["commit", "-m", "initial"]), "git commit");
    must(git(&work, &["branch", "-M", "main"]), "git branch -M main");

    // 5. Write a session blob via dkod_core directly.
    let session = dkod_core::Session {
        id: dkod_core::Session::new_id(),
        agent: dkod_core::Agent::Codex,
        created_at: 1735689600,
        duration_ms: 0,
        prompt_summary: "smoketest fix the bug".into(),
        messages: vec![
            dkod_core::Message::user("smoketest fix the bug"),
            dkod_core::Message::assistant("smoketest done"),
        ],
        commits: vec![],
        files_touched: vec![],
    };
    dkod_core::store::write_session(&work, &session).unwrap();
    let sid = session.id.clone();

    // 6. Confirm log in the working clone shows it.
    let log_local = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&work)
        .arg("log")
        .assert()
        .success();
    let stdout = String::from_utf8(log_local.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(&sid),
        "local dkod log missing session: {stdout}"
    );

    // 7. Push main + refs/dkod/* to origin.
    must(git(&work, &["push", "origin", "main"]), "git push main");
    must(
        git(&work, &["push", "origin", "+refs/dkod/*:refs/dkod/*"]),
        "git push refs/dkod/*",
    );

    // 8. Fresh clone.
    let mut clone_cmd = Std::new("git");
    clone_cmd.args(["clone", bare.to_str().unwrap(), other.to_str().unwrap()]);
    must(clone_cmd, "git clone");

    // 9. The fresh clone's `dkod log` should be empty until we explicitly
    //    fetch the dkod refs (clone does NOT pull non-default refs).
    let log_clone_pre = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&other)
        .arg("log")
        .assert()
        .success();
    let pre_stdout = String::from_utf8(log_clone_pre.get_output().stdout.clone()).unwrap();
    assert!(
        !pre_stdout.contains(&sid),
        "fresh clone should NOT see session before explicit fetch (got: {pre_stdout})",
    );

    // 10. Explicitly fetch refs/dkod/* into the fresh clone.
    must(
        git(&other, &["fetch", "origin", "+refs/dkod/*:refs/dkod/*"]),
        "git fetch refs/dkod/*",
    );

    // 11. Now dkod log + show should work in the fresh clone.
    let log_clone = Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&other)
        .arg("log")
        .assert()
        .success();
    let post_stdout = String::from_utf8(log_clone.get_output().stdout.clone()).unwrap();
    assert!(
        post_stdout.contains(&sid),
        "fresh clone dkod log missing session: {post_stdout}",
    );

    Command::cargo_bin("dkod")
        .unwrap()
        .current_dir(&other)
        .args(["show", &sid])
        .assert()
        .success()
        .stdout(contains("smoketest fix the bug"))
        .stdout(contains("smoketest done"));
}
