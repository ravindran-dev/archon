//! Captures a spawned child's stdout/stderr in the background so a
//! spinner can stand in for it, without risking deadlock: if nothing
//! drains a pipe while the child fills its buffer (a real `makepkg` build
//! prints a lot), the child blocks on its next write and the whole
//! command hangs. Draining happens on dedicated threads regardless of
//! whether the caller is doing a plain blocking `wait()` or an
//! interleaved poll loop (the eBPF monitor's `try_wait()` loop).

use std::io::Read;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;

fn spawn_drain(buf: Arc<Mutex<Vec<u8>>>, mut pipe: impl Read + Send + 'static) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = Vec::new();
        let _ = pipe.read_to_end(&mut chunk);
        buf.lock().unwrap().extend(chunk);
    })
}

pub struct OutputCapture {
    buf: Arc<Mutex<Vec<u8>>>,
    stdout_handle: Option<JoinHandle<()>>,
    stderr_handle: Option<JoinHandle<()>>,
}

impl OutputCapture {
    /// Takes `child`'s stdout/stderr pipes and starts draining them into a
    /// shared buffer. The caller must have spawned `child` with both set
    /// to `Stdio::piped()` (e.g. via `SandboxRunner::capture_output(true)`
    /// or `Command::stdout/stderr(Stdio::piped())` directly).
    pub fn start(child: &mut Child) -> Self {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stdout_handle = child.stdout.take().map(|p| spawn_drain(buf.clone(), p));
        let stderr_handle = child.stderr.take().map(|p| spawn_drain(buf.clone(), p));
        Self {
            buf,
            stdout_handle,
            stderr_handle,
        }
    }

    /// Joins the reader threads (only meaningful once the child has
    /// exited — its pipes only hit EOF then) and returns everything
    /// captured, lossily decoded.
    pub fn finish(self) -> String {
        if let Some(h) = self.stdout_handle {
            let _ = h.join();
        }
        if let Some(h) = self.stderr_handle {
            let _ = h.join();
        }
        String::from_utf8_lossy(&self.buf.lock().unwrap()).into_owned()
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
