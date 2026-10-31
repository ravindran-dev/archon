//! Captures a spawned child's stdout/stderr in the background so a
//! spinner can stand in for it, without risking deadlock: if nothing
//! drains a pipe while the child fills its buffer (a real `makepkg` build
//! prints a lot), the child blocks on its next write and the whole
//! command hangs. Draining happens on dedicated threads regardless of
//! whether the caller is doing a plain blocking `wait()` or an
//! interleaved poll loop (the eBPF monitor's `try_wait()` loop).

use std::io::{Read, Write};
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;

const OUTPUT_TAIL_LIMIT: usize = 256 * 1024;

struct CapturedOutput {
    tail: Vec<u8>,
    last_output: Instant,
    bytes: u64,
}

impl CapturedOutput {
    fn append(&mut self, chunk: &[u8]) {
        self.bytes += chunk.len() as u64;
        self.last_output = Instant::now();
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > OUTPUT_TAIL_LIMIT {
            self.tail.drain(..self.tail.len() - OUTPUT_TAIL_LIMIT);
        }
    }

    fn message(&self, label: &str) -> String {
        let quiet = self.last_output.elapsed().as_secs();
        let tail = String::from_utf8_lossy(&self.tail);
        let last = tail
            .split(['\n', '\r'])
            .rev()
            .find(|s| !s.trim().is_empty())
            .unwrap_or("");
        // Do not relay terminal control sequences from package scripts.
        let mut in_escape = false;
        let detail: String = last
            .chars()
            .filter(|c| {
                if *c == '\u{1b}' {
                    in_escape = true;
                    return false;
                }
                if in_escape {
                    if c.is_ascii_alphabetic() {
                        in_escape = false;
                    }
                    return false;
                }
                !c.is_control()
            })
            .take(120)
            .collect();
        format!(
            "{label} · {} KiB output · no output for {quiet}s · {detail}",
            self.bytes / 1024
        )
    }
}

fn spawn_drain(
    buf: Arc<Mutex<CapturedOutput>>,
    mut pipe: impl Read + Send + 'static,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.lock().unwrap().append(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

pub struct OutputCapture {
    buf: Arc<Mutex<CapturedOutput>>,
    stdout_handle: Option<JoinHandle<()>>,
    stderr_handle: Option<JoinHandle<()>>,
    last_log: Mutex<Instant>,
}

impl OutputCapture {
    /// Takes `child`'s stdout/stderr pipes and starts draining them into a
    /// shared buffer. The caller must have spawned `child` with both set
    /// to `Stdio::piped()` (e.g. via `SandboxRunner::capture_output(true)`
    /// or `Command::stdout/stderr(Stdio::piped())` directly).
    pub fn start(child: &mut Child) -> Self {
        let buf = Arc::new(Mutex::new(CapturedOutput {
            tail: Vec::new(),
            last_output: Instant::now(),
            bytes: 0,
        }));
        let stdout_handle = child.stdout.take().map(|p| spawn_drain(buf.clone(), p));
        let stderr_handle = child.stderr.take().map(|p| spawn_drain(buf.clone(), p));
        Self {
            buf,
            stdout_handle,
            stderr_handle,
            last_log: Mutex::new(Instant::now()),
        }
    }

    pub fn update_progress(&self, spinner: &crate::spinner::Spinner, label: &str) {
        let message = self.buf.lock().unwrap().message(label);
        spinner.set_message(message.clone());
        let mut last_log = self.last_log.lock().unwrap();
        if last_log.elapsed() >= Duration::from_secs(30) {
            use std::io::IsTerminal;
            if !std::io::stderr().is_terminal() {
                let _ = writeln!(std::io::stderr(), "{message}");
            }
            *last_log = Instant::now();
        }
    }

    pub fn wait_with_progress(
        &self,
        child: &mut Child,
        spinner: &crate::spinner::Spinner,
        label: &str,
    ) -> Result<ExitStatus> {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            self.update_progress(spinner, label);
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Joins the reader threads (only meaningful once the child has
    /// exited — its pipes only hit EOF then) and returns the retained
    /// output tail, lossily decoded.
    pub fn finish(self) -> String {
        if let Some(h) = self.stdout_handle {
            let _ = h.join();
        }
        if let Some(h) = self.stderr_handle {
            let _ = h.join();
        }
        String::from_utf8_lossy(&self.buf.lock().unwrap().tail).into_owned()
    }
}

/// Spawn-and-forget capture for callers with no interleaved polling loop
/// (a plain unmonitored build, `git clone`, ...): blocks until the child
/// exits, then returns its status and everything it printed.
pub fn wait_capturing(mut child: Child) -> Result<(ExitStatus, String)> {
    let capture = OutputCapture::start(&mut child);
    let status = child.wait()?;
    Ok((status, capture.finish()))
}

/// Prints captured subprocess output, but only if there's any — a clean
/// exit with nothing on stderr shouldn't add a blank section.
pub fn print_if_nonempty(output: &str) {
    let trimmed = output.trim_end();
    if !trimmed.is_empty() {
        println!("{trimmed}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn output_is_visible_while_child_is_still_running() {
        let mut child = Command::new("sh")
            .args([
                "-c",
                "printf 'Compiling coolercontrold\\n'; read answer; printf 'done\\n' >&2",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let capture = OutputCapture::start(&mut child);
        let deadline = Instant::now() + Duration::from_secs(5);
        while capture.buf.lock().unwrap().bytes == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_none());
        assert!(
            capture
                .buf
                .lock()
                .unwrap()
                .message("Building")
                .contains("Compiling coolercontrold")
        );
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"continue\n")
            .unwrap();
        assert!(child.wait().unwrap().success());
        let output = capture.finish();
        assert!(output.contains("Compiling coolercontrold"));
        assert!(output.contains("done"));
    }

    #[test]
    fn noisy_builds_keep_a_bounded_tail_and_total_activity() {
        let mut output = CapturedOutput {
            tail: Vec::new(),
            bytes: 0,
            last_output: Instant::now(),
        };
        output.append(&vec![b'x'; OUTPUT_TAIL_LIMIT]);
        output.append(b"\nlatest compiler message\n");
        assert_eq!(output.tail.len(), OUTPUT_TAIL_LIMIT);
        assert!(output.bytes > OUTPUT_TAIL_LIMIT as u64);
        assert!(
            output
                .message("Building")
                .ends_with("latest compiler message")
        );
    }

    #[test]
    fn progress_removes_terminal_escape_sequences() {
        let mut output = CapturedOutput {
            tail: Vec::new(),
            bytes: 0,
            last_output: Instant::now(),
        };
        output.append(b"\x1b[31mCompiling\x1b[0m\n");
        let message = output.message("Building");
        assert!(message.ends_with("Compiling"));
        assert!(!message.contains('\x1b'));
    }
}
