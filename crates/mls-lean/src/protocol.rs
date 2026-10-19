//! Blank-line framed JSON transport for the persistent community Lean REPL.

use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

/// Process and resource limits for one persistent REPL.
#[derive(Clone, Debug)]
pub struct ReplConfig {
    /// Executable to launch (normally the pinned REPL binary).
    pub program: PathBuf,
    /// Arguments passed verbatim to the executable.
    pub args: Vec<String>,
    /// Working directory containing the pinned shadow Lake project.
    pub cwd: PathBuf,
    /// Additional child environment entries.
    pub env: Vec<(String, String)>,
    /// Maximum time to wait for a response frame.
    pub timeout: Duration,
    /// Maximum bytes in one stdout frame.
    pub max_stdout_bytes: usize,
    /// Maximum retained stderr bytes (oldest bytes are discarded).
    pub max_stderr_bytes: usize,
}

/// A process, framing, timeout, or JSON protocol failure.
#[derive(Debug, thiserror::Error)]
pub enum ReplError {
    /// The child could not be spawned or communicated with.
    #[error("Lean REPL I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// No complete response arrived within the configured deadline.
    #[error("Lean REPL timed out after {0:?}")]
    Timeout(Duration),
    /// The child closed stdout before returning a complete frame.
    #[error("Lean REPL closed stdout: {stderr}")]
    Eof {
        /// Bounded tail of child stderr.
        stderr: String,
    },
    /// A response exceeded the configured bound.
    #[error("Lean REPL stdout frame exceeded {limit} bytes")]
    StdoutLimit {
        /// Configured maximum frame size.
        limit: usize,
    },
    /// A framed response was not a JSON object.
    #[error("invalid Lean REPL JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The REPL returned its outer protocol error object.
    #[error("Lean REPL protocol error: {0}")]
    Protocol(String),
}

pub(crate) struct ReplProcess {
    child: Child,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Result<Vec<u8>, FrameError>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    config: ReplConfig,
}

enum FrameError {
    Eof,
    Limit,
    Io(std::io::Error),
}

impl ReplProcess {
    pub(crate) fn spawn(config: ReplConfig) -> Result<Self, ReplError> {
        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .current_dir(&config.cwd)
            .envs(config.env.iter().cloned())
            .env_remove("OPENROUTER_API_KEY");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdout"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("missing stderr"))?;

        let (sender, frames) = mpsc::channel();
        let limit = config.max_stdout_bytes;
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut frame = Vec::new();
                loop {
                    let mut byte = [0_u8; 1];
                    use std::io::Read as _;
                    match reader.read(&mut byte) {
                        Ok(0) => {
                            let _ = sender.send(Err(FrameError::Eof));
                            return;
                        }
                        Ok(_) => {
                            frame.push(byte[0]);
                            let delimiter = if frame.ends_with(b"\n\n") {
                                Some(2)
                            } else if frame.ends_with(b"\r\n\r\n") {
                                Some(4)
                            } else {
                                None
                            };
                            if let Some(length) = delimiter {
                                frame.truncate(frame.len() - length);
                                break;
                            }
                            if frame.len() > limit {
                                let _ = sender.send(Err(FrameError::Limit));
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(FrameError::Io(error)));
                            return;
                        }
                    }
                }
                if sender.send(Ok(frame)).is_err() {
                    return;
                }
            }
        });

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_clone = Arc::clone(&stderr);
        let stderr_limit = config.max_stderr_bytes;
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr_pipe);
            let mut chunk = [0_u8; 4096];
            loop {
                use std::io::Read as _;
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(count) => {
                        let mut retained = stderr_clone.lock().expect("stderr mutex poisoned");
                        retained.extend(&chunk[..count]);
                        while retained.len() > stderr_limit {
                            retained.pop_front();
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            frames,
            stderr,
            config,
        })
    }

    pub(crate) fn request(&mut self, request: &Value) -> Result<Value, ReplError> {
        serde_json::to_writer(&mut self.stdin, request)?;
        self.stdin.write_all(b"\n\n")?;
        self.stdin.flush()?;
        let frame = match self.frames.recv_timeout(self.config.timeout) {
            Ok(Ok(frame)) => frame,
            Ok(Err(FrameError::Eof)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ReplError::Eof {
                    stderr: self.stderr_text(),
                });
            }
            Ok(Err(FrameError::Limit)) => {
                let _ = self.child.kill();
                return Err(ReplError::StdoutLimit {
                    limit: self.config.max_stdout_bytes,
                });
            }
            Ok(Err(FrameError::Io(error))) => return Err(ReplError::Io(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The request may still complete later, so the byte stream can no longer
                // be correlated safely with future requests. Poison this worker process.
                let _ = self.child.kill();
                return Err(ReplError::Timeout(self.config.timeout));
            }
        };
        let response: Value = serde_json::from_slice(&frame)?;
        if let Some(message) = response.get("message").and_then(Value::as_str) {
            return Err(ReplError::Protocol(message.to_owned()));
        }
        if !response.is_object() {
            return Err(ReplError::Protocol("response is not an object".into()));
        }
        Ok(response)
    }

    fn stderr_text(&self) -> String {
        let bytes: Vec<_> = self
            .stderr
            .lock()
            .expect("stderr mutex poisoned")
            .iter()
            .copied()
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Drop for ReplProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
