//! Captures a spawned child's stdout/stderr in the background so a
//! spinner can stand in for it, without risking deadlock: if nothing
//! drains a pipe while the child fills its buffer (a real `makepkg` build
//! prints a lot), the child blocks on its next write and the whole
//! command hangs. Draining happens on dedicated threads regardless of
//! whether the caller is doing a plain blocking `wait()` or an
//! interleaved poll loop (the eBPF monitor's `try_wait()` loop).

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;

const OUTPUT_TAIL_LIMIT: usize = 256 * 1024;

struct CapturedOutput {
    tail: Vec<u8>,
    last_output: Instant,
    bytes: u64,
    read_error: Option<String>,
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
    mut pipe: impl Read + AsRawFd + Send + 'static,
    stopping: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        let mut after_stop = 0usize;
        loop {
            let stop = stopping.load(Ordering::Acquire);
            let mut descriptor = libc::pollfd {
                fd: pipe.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut descriptor, 1, if stop { 0 } else { 100 }) };
            if ready == 0 {
                if stop {
                    break;
                } else {
                    continue;
                }
            }
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                buf.lock().unwrap().read_error = Some(error.to_string());
                break;
            }
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.lock().unwrap().append(&chunk[..n]);
                    if stop {
                        after_stop += n;
                        if after_stop >= OUTPUT_TAIL_LIMIT {
                            break;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    buf.lock().unwrap().read_error = Some(e.to_string());
                    break;
                }
            }
        }
    })
}

pub struct OutputCapture {
    buf: Arc<Mutex<CapturedOutput>>,
    stdout_handle: Option<JoinHandle<()>>,
    stderr_handle: Option<JoinHandle<()>>,
    last_log: Mutex<Instant>,
    stopping: Arc<AtomicBool>,
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
            read_error: None,
        }));
        let stopping = Arc::new(AtomicBool::new(false));
        let stdout_handle = child
            .stdout
            .take()
            .map(|p| spawn_drain(buf.clone(), p, stopping.clone()));
        let stderr_handle = child
            .stderr
            .take()
            .map(|p| spawn_drain(buf.clone(), p, stopping.clone()));
        Self {
            buf,
            stdout_handle,
            stderr_handle,
            last_log: Mutex::new(Instant::now()),
            stopping,
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
    pub fn finish(mut self) -> Result<String> {
        self.stopping.store(true, Ordering::Release);
        if let Some(h) = self.stdout_handle.take() {
            let _ = h.join();
        }
        if let Some(h) = self.stderr_handle.take() {
            let _ = h.join();
        }
        let output = self.buf.lock().unwrap();
        if let Some(error) = &output.read_error {
            anyhow::bail!("failed to capture subprocess output: {error}");
        }
        Ok(String::from_utf8_lossy(&output.tail).into_owned())
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
    }
}

/// Reap failed subprocesses when a caller exits early (for example on a monitor
/// or log error). Bubblewrap's PID namespace owns the build descendants.
pub struct ManagedChild(Child);
impl ManagedChild {
    pub fn new(child: Child) -> Self {
        Self(child)
    }
}
impl std::ops::Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
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
        let output = capture.finish().unwrap();
        assert!(output.contains("Compiling coolercontrold"));
        assert!(output.contains("done"));
    }

    #[test]
    fn noisy_builds_keep_a_bounded_tail_and_total_activity() {
        let mut output = CapturedOutput {
            tail: Vec::new(),
            bytes: 0,
            last_output: Instant::now(),
            read_error: None,
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
            read_error: None,
        };
        output.append(b"\x1b[31mCompiling\x1b[0m\n");
        let message = output.message("Building");
        assert!(message.ends_with("Compiling"));
        assert!(!message.contains('\x1b'));
    }
    #[test]
    fn inherited_output_pipes_do_not_hang_after_parent_exits() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 2 & printf 'finished\\n'"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let capture = OutputCapture::start(&mut child);
        assert!(child.wait().unwrap().success());
        let start = Instant::now();
        assert!(capture.finish().unwrap().contains("finished"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn early_exit_kills_and_reaps_the_child() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        drop(ManagedChild::new(child));
        let result = unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(result, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
