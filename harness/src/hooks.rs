//! Runs setup, ready, check and teardown commands with timeouts and captures
//! their output. Every child starts with an explicit allowlisted environment,
//! so a task cannot leak payment credentials into a coding agent: only `PATH`,
//! `HOME`, `TERM` and the variables the task names are passed through.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// What a command left behind.
#[derive(Debug, Clone)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub ms: u128,
    /// The command was killed at its timeout.
    pub timed_out: bool,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// The last few lines of stderr, for a finding an agent can act on.
    pub fn tail(&self, lines: usize) -> String {
        let all: Vec<&str> = self
            .stderr
            .lines()
            .chain(self.stdout.lines())
            .filter(|l| !l.trim().is_empty())
            .collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("{command}: {problem}")]
    Spawn { command: String, problem: String },
}

/// The variables every child receives, whatever the environment holds.
pub const ALLOWED: [&str; 3] = ["PATH", "HOME", "TERM"];

/// Runs one command under `sh -c` in `dir`, with `extra` added to the
/// allowlisted environment. Returns what it left behind, even on failure:
/// classifying the failure is the caller's job.
pub async fn run(
    command: &str,
    dir: &std::path::Path,
    extra: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Output, HookError> {
    let started = Instant::now();
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ALLOWED {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    // The child leads its own process group, so a timeout can end everything
    // it started, not just the shell. A payment loop that outlived its check
    // could otherwise keep submitting after Proctor reported failure.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| HookError::Spawn {
        command: command.to_owned(),
        problem: e.to_string(),
    })?;
    let group = child.id().map(|pid| pid as i32);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader = async move {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let a = async {
            if let Some(mut s) = stdout {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut out).await;
            }
            out
        };
        let b = async {
            if let Some(mut s) = stderr {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut err).await;
            }
            err
        };
        tokio::join!(a, b)
    };
    let wait = async {
        // Drain both pipes while the child runs. Waiting first deadlocks as
        // soon as a compiler or JSON report fills either pipe.
        let (status, (out, err)) = tokio::join!(child.wait(), reader);
        (status, out, err)
    };
    match tokio::time::timeout(timeout, wait).await {
        Ok((Ok(status), out, err)) => Ok(Output {
            code: status.code(),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            ms: started.elapsed().as_millis(),
            timed_out: false,
        }),
        Ok((Err(e), _, _)) => Err(HookError::Spawn {
            command: command.to_owned(),
            problem: e.to_string(),
        }),
        // A timeout is never a pass, and never leaves work running behind it.
        Err(_) => {
            let killed = kill_group(group).await;
            // Reap the direct child before returning to evidence collection.
            let _ = child.wait().await;
            Ok(Output {
                code: None,
                stdout: String::new(),
                stderr: format!("timed out after {} s; {killed}", timeout.as_secs()),
                ms: started.elapsed().as_millis(),
                timed_out: true,
            })
        }
    }
}

/// Ends a timed-out command and everything it started: `SIGTERM` to the
/// group, a moment to exit, then `SIGKILL`. Returns what happened, for the
/// finding.
async fn kill_group(group: Option<i32>) -> String {
    let Some(pid) = group else {
        return "the process had already exited".to_owned();
    };
    // Negative pid signals the group the child leads.
    let signal = |sig: i32| unsafe { libc::kill(-pid, sig) };
    if signal(libc::SIGTERM) == -1 {
        return "the process group had already exited".to_owned();
    }
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        // ESRCH means nothing in the group is left.
        if unsafe { libc::kill(-pid, 0) } == -1 {
            return format!("process group {pid} terminated");
        }
    }
    signal(libc::SIGKILL);
    format!("process group {pid} killed")
}

/// Polls `command` until it succeeds or the timeout passes: the fixture is up.
pub async fn wait_ready(
    command: &str,
    dir: &std::path::Path,
    extra: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Output, HookError> {
    let deadline = Instant::now() + timeout;
    let mut last = run(command, dir, extra, timeout.min(Duration::from_secs(10))).await?;
    while !last.ok() && Instant::now() < deadline {
        tokio::time::sleep(
            Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())),
        )
        .await;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        last = run(command, dir, extra, remaining.min(Duration::from_secs(10))).await?;
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    #[tokio::test]
    async fn a_command_reports_what_it_left_behind() {
        let none = BTreeMap::new();
        let out = run(
            "echo hello; echo oops >&2",
            &here(),
            &none,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(out.ok());
        assert_eq!(out.stdout.trim(), "hello");
        assert_eq!(out.stderr.trim(), "oops");
        let failed = run("exit 3", &here(), &none, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(failed.code, Some(3));
        assert!(!failed.ok());
    }

    #[tokio::test]
    async fn drains_large_stdout_and_stderr_before_waiting() {
        let out = run(
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
            &here(),
            &BTreeMap::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(out.ok(), "{}", out.stderr);
        assert_eq!(out.stdout.len(), 1048576);
        assert_eq!(out.stderr.len(), 1048576);
    }

    #[tokio::test]
    async fn a_timeout_is_never_a_pass() {
        let none = BTreeMap::new();
        let out = run("sleep 5", &here(), &none, Duration::from_millis(200))
            .await
            .unwrap();
        assert!(out.timed_out);
        assert!(!out.ok(), "a timeout never passes");
        assert!(out.stderr.contains("timed out"));
    }

    /// A timed-out command must leave nothing running. A payment loop that
    /// outlived its check could keep submitting after Proctor reported
    /// failure, so the whole process group goes, not just the shell.
    #[tokio::test]
    async fn a_timeout_leaves_nothing_running() {
        let dir = std::env::temp_dir().join(format!("proctor-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("still-running");
        std::fs::remove_file(&marker).ok();
        let none = BTreeMap::new();
        // A grandchild that would write well after the timeout.
        let out = run(
            &format!("(sleep 1; touch {}) & wait", marker.display()),
            &dir,
            &none,
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert!(out.timed_out);
        assert!(
            out.stderr.contains("terminated") || out.stderr.contains("killed"),
            "the finding says what happened: {}",
            out.stderr
        );
        assert!(
            !marker.exists(),
            "nothing wrote at the moment Proctor returned"
        );
        // And nothing writes afterwards either.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !marker.exists(),
            "a descendant survived the timeout and kept working"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_child_sees_only_the_allowlist_and_what_the_task_names() {
        // Something that is in this process's environment but not allowlisted.
        unsafe { std::env::set_var("MANDATE_PRIVATE_KEY", "must-not-leak") };
        let mut extra = BTreeMap::new();
        extra.insert("TASK_VAR".to_owned(), "visible".to_owned());
        let out = run(
            "echo key=[${MANDATE_PRIVATE_KEY:-}] task=[${TASK_VAR:-}] path=[${PATH:+set}]",
            &here(),
            &extra,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(
            out.stdout.contains("key=[]"),
            "the key never reaches a child: {}",
            out.stdout
        );
        assert!(out.stdout.contains("task=[visible]"));
        assert!(out.stdout.contains("path=[set]"));
        unsafe { std::env::remove_var("MANDATE_PRIVATE_KEY") };
    }

    #[tokio::test]
    async fn ready_polls_until_the_fixture_answers() {
        let dir = std::env::temp_dir().join(format!("proctor-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flag = dir.join("up");
        std::fs::remove_file(&flag).ok();
        let none = BTreeMap::new();
        // Nothing creates the flag: ready gives up and says so.
        let out = wait_ready(
            &format!("test -f {}", flag.display()),
            &dir,
            &none,
            Duration::from_millis(400),
        )
        .await
        .unwrap();
        assert!(!out.ok());
        std::fs::write(&flag, "1").unwrap();
        let out = wait_ready(
            &format!("test -f {}", flag.display()),
            &dir,
            &none,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(out.ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
