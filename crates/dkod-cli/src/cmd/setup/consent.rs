//! Interactive prompts + non-interactive fallback for the seamless capture
//! wizard.
//!
//! The wizard installs hooks silently for most agents, but a few install
//! paths (notably the Codex PATH shim) need explicit user consent. In a
//! TTY we ask; in CI (`install.sh | sh` piped from a non-interactive
//! shell) we skip and record the deferral in the wizard's config.

use anyhow::{anyhow, Result};
use std::io::{BufRead, IsTerminal, Write};

/// Maximum number of times the interactive prompter will re-ask after
/// invalid input before giving up. Three matches what an attentive user
/// can recover from while keeping a misconfigured pipe from looping.
const MAX_PROMPT_RETRIES: usize = 3;

/// Tiny abstraction over "ask the user a multiple-choice question" so the
/// wizard can run against stdin in production and against an in-memory
/// scripted answer set in tests.
pub trait Prompter {
    /// Display `question`, present `choices`, and return the chosen value.
    /// Implementations may loop on invalid input; on persistent failure
    /// they return an error rather than guessing a default.
    fn ask(&mut self, question: &str, choices: &[&str]) -> Result<String>;
}

/// Returns `true` iff stdin is connected to an interactive terminal. The
/// wizard uses this both to decide whether to construct a `StdinPrompter`
/// at all and to short-circuit `install.sh` when piped from `curl`.
pub fn is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// `Prompter` backed by stdin / stdout. Suitable for the live CLI flow.
pub struct StdinPrompter;

impl Prompter for StdinPrompter {
    fn ask(&mut self, question: &str, choices: &[&str]) -> Result<String> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        ask_via_reader(question, choices, &mut stdin.lock(), &mut stdout)
    }
}

/// `Prompter` that always returns the same answer regardless of input.
/// Used in non-interactive contexts (CI, pipes) where prompting would
/// stall forever waiting on an EOF.
pub struct NonInteractivePrompter {
    default: String,
}

impl NonInteractivePrompter {
    pub fn new(default: impl Into<String>) -> Self {
        Self {
            default: default.into(),
        }
    }
}

impl Prompter for NonInteractivePrompter {
    fn ask(&mut self, _question: &str, choices: &[&str]) -> Result<String> {
        // Default must be a valid choice — silently returning an
        // unrecognised value would let a misconfigured caller drive
        // install-state transitions the prompt never offered. Match
        // case-insensitively so callers can configure `"y"` while the
        // installer accepts `"Y"` / `"yes"` etc.
        if let Some(choice) = choices
            .iter()
            .find(|c| c.eq_ignore_ascii_case(self.default.as_str()))
        {
            return Ok((*choice).to_string());
        }
        Err(anyhow!(
            "non-interactive default {:?} is not in allowed choices: {}",
            self.default,
            choices.join(", ")
        ))
    }
}

/// The actual prompt loop, parameterized by reader + writer so it can be
/// driven against in-memory streams (used by tests and by the
/// non-interactive install.sh path that wants to reuse the formatting).
/// Loops on invalid answers up to `MAX_PROMPT_RETRIES`.
pub fn ask_via_reader<R: BufRead, W: Write>(
    question: &str,
    choices: &[&str],
    reader: &mut R,
    writer: &mut W,
) -> Result<String> {
    if choices.is_empty() {
        return Err(anyhow!("ask() called with no choices"));
    }

    for attempt in 0..=MAX_PROMPT_RETRIES {
        writeln!(writer, "{question}")?;
        writeln!(writer, "Choices: {}", choices.join(" / "))?;
        write!(writer, "> ")?;
        writer.flush()?;

        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Err(anyhow!("stdin closed before answering prompt: {question}"));
        }
        let answer = line.trim();
        if let Some(choice) = choices.iter().find(|c| c.eq_ignore_ascii_case(answer)) {
            return Ok((*choice).to_string());
        }

        if attempt < MAX_PROMPT_RETRIES {
            writeln!(
                writer,
                "Unrecognized answer {answer:?}; please pick one of: {}",
                choices.join(", ")
            )?;
        }
    }

    Err(anyhow!(
        "no valid answer to {question:?} after {} attempts",
        MAX_PROMPT_RETRIES + 1
    ))
}
