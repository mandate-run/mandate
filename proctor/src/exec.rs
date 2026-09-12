//! Run task hooks and checks as child processes with timeouts, capturing
//! output. A hook that runs project code and fails is an implementation
//! failure; a missing toolchain or an unreachable verifier is an
//! infrastructure error.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub exit_ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl RunOutput {
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

pub fn run_shell(command: &str, timeout: Duration, cwd: &Path) -> RunOutput {
    let start = Instant::now();
    let mut child = match Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return RunOutput {
                exit_ok: false,
                code: None,
                stdout: String::new(),
                stderr: format!("spawn failed: {e}"),
                timed_out: false,
            }
        }
    };

    // Poll for completion so we can enforce a timeout without a shell
    // dependency on `timeout`.
    let output = loop {
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break RunOutput {
                exit_ok: false,
                code: None,
                stdout: String::new(),
                stderr: format!("timed out after {timeout:?}"),
                timed_out: true,
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                let exit_ok = status.success();
                // Wait for pipes to flush after try_wait reported exit.
                let output = child.wait_with_output();
                match output {
                    Ok(o) => {
                        break RunOutput {
                            exit_ok,
                            code,
                            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
                            timed_out: false,
                        }
                    }
                    Err(e) => {
                        break RunOutput {
                            exit_ok: false,
                            code,
                            stdout: String::new(),
                            stderr: format!("read output: {e}"),
                            timed_out: false,
                        }
                    }
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                break RunOutput {
                    exit_ok: false,
                    code: None,
                    stdout: String::new(),
                    stderr: format!("wait failed: {e}"),
                    timed_out: false,
                }
            }
        }
    };
    output
}
