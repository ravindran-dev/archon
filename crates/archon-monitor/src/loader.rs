//! Loads the compiled eBPF object and drives it from userspace.
//!
//! Everything in this module requires root (or `CAP_BPF`+`CAP_PERFMON`) to
//! actually run — loading a program into the kernel and attaching a
//! tracepoint are privileged operations. Callers must gate this behind an
//! explicit confirmation step; nothing here should run unattended on a
//! user's daily-driver machine. It's also the one module in this crate
//! that genuinely cannot be unit-tested without a live kernel and root, so
//! there are no `#[cfg(test)]` cases below — correctness here is verified
//! by actually loading and attaching against a running kernel.

use std::path::{Path, PathBuf};

use aya::maps::{Array as AyaArray, MapData, RingBuf as AyaRingBuf};
use aya::programs::TracePoint;
use aya::Ebpf;
use aya_log::EbpfLogger;

use archon_monitor_common::SyscallEvent;

#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    #[error("failed to load eBPF object: {0}")]
    Load(#[from] aya::EbpfError),
    #[error("failed to load/attach program {name}: {source}")]
    Program {
        name: &'static str,
        #[source]
        source: aya::programs::ProgramError,
    },
    #[error("map {0} not found in eBPF object (object/loader version mismatch?)")]
    MapNotFound(&'static str),
    #[error("map error: {0}")]
    Map(#[from] aya::maps::MapError),
    #[error("program {0} not found or is not a tracepoint (object/loader version mismatch?)")]
    ProgramNotFound(&'static str),
}

/// (eBPF program symbol, tracepoint category, tracepoint name).
const TRACEPOINTS: &[(&str, &str, &str)] = &[
    ("archon_sys_enter_execve", "syscalls", "sys_enter_execve"),
    ("archon_sys_enter_openat", "syscalls", "sys_enter_openat"),
    ("archon_sys_enter_connect", "syscalls", "sys_enter_connect"),
];

pub struct Monitor {
    ebpf: Ebpf,
    /// Bridges `aya_log_ebpf::debug!`/etc. calls in the eBPF program to
    /// the `log` crate on the userspace side. `None` if initialization
    /// failed (e.g. the object was built without the debug logging map) —
    /// non-fatal, monitoring itself doesn't depend on it.
    logger: Option<EbpfLogger<&'static dyn log::Log>>,
}

impl Monitor {
    /// Loads the compiled eBPF object and attaches every tracepoint.
    /// Requires root. Nothing is scoped to a particular build yet — call
    /// [`Monitor::set_target_cgroup`] before starting the sandboxed
    /// process, or every probe no-ops (cgroup ID `0` never matches a real
    /// cgroup, which is the target's zero-value before it's set).
    pub fn load_and_attach(obj_path: &Path) -> Result<Self, MonitorError> {
        let mut ebpf = Ebpf::load_file(obj_path)?;
        // Best-effort: routes the eBPF program's debug!/info! calls to the
        // `log` crate (the CLI wires that to env_logger — set RUST_LOG=debug
        // to see them). Never fatal; monitoring works without it.
        let logger = EbpfLogger::init(&mut ebpf).ok();
        for (prog_name, category, name) in TRACEPOINTS {
            let program: &mut TracePoint = ebpf
                .program_mut(prog_name)
                .ok_or(MonitorError::ProgramNotFound(prog_name))?
                .try_into()
                .map_err(|source| MonitorError::Program { name: prog_name, source })?;
            program
                .load()
                .map_err(|source| MonitorError::Program { name: prog_name, source })?;
            program
                .attach(category, name)
                .map_err(|source| MonitorError::Program { name: prog_name, source })?;
        }
        Ok(Self { ebpf, logger })
    }

    /// Flushes any buffered log entries from [`EbpfLogger`] out to the
    /// `log` crate. Call periodically alongside [`Monitor::drain_events`].
    pub fn flush_logs(&mut self) {
        if let Some(logger) = &mut self.logger {
            logger.flush();
        }
    }

    /// Points the monitor at the cgroup the sandboxed build is (about to
    /// be) moved into. Cgroup membership is inherited through `fork()` by
    /// the kernel, so every descendant the build spawns is covered
    /// automatically — no per-process bookkeeping needed on our side.
    pub fn set_target_cgroup(&mut self, cgroup_id: u64) -> Result<(), MonitorError> {
        let map = self
            .ebpf
            .map_mut("TARGET_CGROUP")
            .ok_or(MonitorError::MapNotFound("TARGET_CGROUP"))?;
        let mut target: AyaArray<&mut MapData, u64> = AyaArray::try_from(map)?;
        target.set(0, cgroup_id, 0)?;
        Ok(())
    }

    /// Drains every event currently sitting in the ring buffer without
    /// blocking. Call periodically while the sandboxed build runs, and
    /// once more after it exits to catch anything written in the final
    /// moments before teardown.
    pub fn drain_events(&mut self) -> Result<Vec<SyscallEvent>, MonitorError> {
        let map = self
            .ebpf
            .map_mut("EVENTS")
            .ok_or(MonitorError::MapNotFound("EVENTS"))?;
        let mut ring: AyaRingBuf<&mut MapData> = AyaRingBuf::try_from(map)?;
        let mut events = Vec::new();
        while let Some(item) = ring.next() {
            if let Some(event) = unsafe { SyscallEvent::from_bytes(&item) } {
                events.push(event);
            }
        }
        Ok(events)
    }
}

/// Resolves the compiled eBPF object's path: the `ARCHON_EBPF_OBJECT` env
/// var if set, otherwise the dev-tree build output relative to this crate
/// (only present after `cargo +nightly build --release` inside
/// `crates/archon-monitor-ebpf` — see that crate's doc comment).
pub fn default_object_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ARCHON_EBPF_OBJECT") {
        return Some(PathBuf::from(p));
    }
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../archon-monitor-ebpf/target/bpfel-unknown-none/release/archon-monitor-ebpf",
    );
    dev_path.exists().then_some(dev_path)
}
