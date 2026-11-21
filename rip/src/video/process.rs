// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs a child program, hands its output over line by line, and stops it
//! (with everything it started) when the user cancels.

use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{Result, VideoError};

/// What a finished child left behind.
#[derive(Debug)]
pub struct Finished {
    /// How it ended.
    pub status: ExitStatus,
    /// The last lines it wrote to its error output.
    pub stderr: Vec<String>,
}

/// Runs `command`, calling `on_line` for every line of standard output
/// (the error output is kept, a few lines of it, for the caller to quote).
/// When `cancel` is set the child and its children are killed and
/// [`VideoError::Cancelled`] comes back. Dropping the future kills it too.
pub async fn run(
    mut command: Command,
    program: &str,
    cancel: &AtomicBool,
    mut on_line: impl FnMut(&str),
) -> Result<Finished> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command.spawn().map_err(|source| VideoError::Start {
        program: program.to_owned(),
        source,
    })?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let errors = tokio::spawn(async move {
        let mut last: Vec<String> = Vec::new();
        if let Some(stderr) = stderr {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if last.len() == 8 {
                    last.remove(0);
                }
                last.push(line);
            }
        }
        last
    });
    let mut lines = stdout.map(|out| BufReader::new(out).lines());
    let mut stopped = false;
    while let Some(reader) = lines.as_mut() {
        tokio::select! {
            line = reader.next_line() => match line {
                Ok(Some(line)) => on_line(&line),
                Ok(None) | Err(_) => break,
            },
            () = wait_for(cancel) => {
                stopped = true;
                break;
            }
        }
    }
    if stopped {
        kill_tree(pid);
        let _ = child.kill().await;
        let _ = child.wait().await;
        errors.abort();
        return Err(VideoError::Cancelled);
    }
    let status = tokio::select! {
        status = child.wait() => status.map_err(|source| VideoError::Start {
            program: program.to_owned(),
            source,
        })?,
        () = wait_for(cancel) => {
            kill_tree(pid);
            let _ = child.kill().await;
            let _ = child.wait().await;
            errors.abort();
            return Err(VideoError::Cancelled);
        }
    };
    let stderr = errors.await.unwrap_or_default();
    Ok(Finished { status, stderr })
}

async fn wait_for(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(unix)]
fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // The child leads its own process group (see `process_group(0)`).
        // SAFETY: kill(2) with a group id of a child this process started.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
}

#[cfg(windows)]
fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lines_arrive_and_the_exit_status_is_kept() {
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let done = run(
            shell("echo one; echo two; echo oops >&2; exit 3"),
            "sh",
            &cancel,
            |l| {
                seen.push(l.to_owned());
            },
        )
        .await
        .unwrap();
        assert_eq!(seen, ["one", "two"]);
        assert_eq!(done.status.code(), Some(3));
        assert_eq!(done.stderr, ["oops"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_child_and_what_it_started() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            flag.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let result = run(
            shell("sleep 30 & sleep 30; echo late"),
            "sh",
            &cancel,
            |_| {},
        )
        .await;
        assert!(matches!(result, Err(VideoError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_missing_program_says_which() {
        let cancel = AtomicBool::new(false);
        let result = run(
            Command::new("definitely-not-a-program-xyz"),
            "definitely-not-a-program-xyz",
            &cancel,
            |_| {},
        )
        .await;
        assert!(matches!(result, Err(VideoError::Start { .. })));
    }
}
