//! Tests for `cmd::setup::consent` — wizard's prompt abstraction.

use dkod_cli::cmd::setup::consent::{ask_via_reader, NonInteractivePrompter, Prompter};
use std::io::Cursor;

#[test]
fn non_interactive_returns_default_regardless_of_input() {
    let mut prompter = NonInteractivePrompter::new("y");
    let answer = prompter.ask("install shim?", &["y", "n", "never"]).unwrap();
    assert_eq!(answer, "y");

    // Same default, different question — still returns the same default.
    let answer = prompter.ask("anything else?", &["a", "b"]).unwrap();
    assert_eq!(answer, "y");
}

#[test]
fn stdin_prompter_returns_user_choice() {
    let input = b"y\n";
    let mut reader = Cursor::new(&input[..]);
    let mut writer: Vec<u8> = Vec::new();

    let answer = ask_via_reader("install shim?", &["y", "n"], &mut reader, &mut writer).unwrap();
    assert_eq!(answer, "y");

    let printed = String::from_utf8(writer).unwrap();
    assert!(printed.contains("install shim?"));
    assert!(printed.contains("Choices:"));
}

#[test]
fn stdin_prompter_is_case_insensitive() {
    let input = b"NEVER\n";
    let mut reader = Cursor::new(&input[..]);
    let mut writer: Vec<u8> = Vec::new();

    let answer = ask_via_reader(
        "install shim?",
        &["y", "n", "never"],
        &mut reader,
        &mut writer,
    )
    .unwrap();
    assert_eq!(answer, "never");
}

#[test]
fn stdin_prompter_loops_on_invalid_input_then_succeeds() {
    let input = b"maybe\nperhaps\nn\n";
    let mut reader = Cursor::new(&input[..]);
    let mut writer: Vec<u8> = Vec::new();

    let answer = ask_via_reader("install shim?", &["y", "n"], &mut reader, &mut writer).unwrap();
    assert_eq!(answer, "n");

    let printed = String::from_utf8(writer).unwrap();
    // Should have re-prompted at least twice with "Unrecognized answer".
    assert!(
        printed.matches("Unrecognized answer").count() >= 2,
        "expected at least two re-prompts, output was:\n{printed}"
    );
}

#[test]
fn stdin_prompter_errors_after_max_retries_of_bad_input() {
    // 4 bad answers; we tolerate at most MAX_PROMPT_RETRIES = 3 retries
    // (i.e. 4 attempts total), so a 5th bad answer should not be needed —
    // we just need to exhaust the budget.
    let input = b"x\nx\nx\nx\n";
    let mut reader = Cursor::new(&input[..]);
    let mut writer: Vec<u8> = Vec::new();

    let err = ask_via_reader("install shim?", &["y", "n"], &mut reader, &mut writer)
        .expect_err("should give up after max retries");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("retries") || msg.contains("no valid answer"),
        "expected retry-exhausted error, got: {msg}"
    );
}

#[test]
fn stdin_prompter_errors_on_closed_stdin() {
    let input: &[u8] = b"";
    let mut reader = Cursor::new(input);
    let mut writer: Vec<u8> = Vec::new();

    let err = ask_via_reader("install shim?", &["y", "n"], &mut reader, &mut writer)
        .expect_err("should fail when stdin closes immediately");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("stdin closed") || msg.contains("before answering"),
        "expected closed-stdin error, got: {msg}"
    );
}
