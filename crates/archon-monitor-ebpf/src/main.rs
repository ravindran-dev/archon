//! eBPF programs monitoring a sandboxed AUR build's process tree.
//!
//! Scoping strategy: a dedicated cgroup, not PID-set propagation.
//! `archon` (running as root to load this program in the first place)
//! creates one cgroup per build and moves the sandboxed `bwrap` process
//! into it before it execs anything; every descendant it forks inherits
//! cgroup membership automatically — the kernel does that propagation for
//! free, correctly, with no tracepoint bookkeeping on our side. Each probe
//! just compares `bpf_get_current_cgroup_id()` against the one target ID
//! userspace wrote into `TARGET_CGROUP`.
//!
//! An earlier version of this file tracked descendants itself via a
//! `sched_process_fork` tracepoint and a `TRACKED_PIDS` map, seeded with
//! only the root PID. That relied on guessed byte offsets into the
//! tracepoint's raw format (this sandboxed user can't read
//! `/sys/kernel/tracing/events/.../format` to confirm them — see the git
//! history for the reasoning). Live testing against this machine's kernel
//! showed the guess was wrong: fork propagation silently never fired, so
//! only bwrap's own setup syscalls were ever observed and everything
//! `makepkg` and its children did afterward was invisible — the tool
//! would have shipped with its actual purpose defeated. Cgroup membership
//! has no such offset to get wrong, which is the whole reason it's the
//! better design once the loader runs as root anyway.
//!
//! Field offsets for the syscall tracepoints below are the well-known,
//! ABI-stable layout used throughout the eBPF ecosystem: every
//! `sys_enter_*` argument occupies a padded 8-byte slot (kernel's
//! `__field(long, ...)` via `include/trace/events/syscalls.h`) regardless
//! of the argument's actual C type width. The live test only exercised
//! `openat` (bwrap's own pre-exec setup calls came through with correct
//! paths, confirming that offset) — it never exercised `execve` or
//! `connect`, because the untracked child described above is exactly the
//! process that would have called them. Those two offsets are still
//! unverified against a live kernel; re-test once the cgroup fix lands.

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::{
        bpf_get_current_cgroup_id, bpf_get_current_comm, bpf_get_current_pid_tgid,
        bpf_ktime_get_ns, bpf_probe_read_user, bpf_probe_read_user_str_bytes,
    },
    macros::{map, tracepoint},
    maps::{Array, RingBuf},
    programs::TracePointContext,
};
use aya_log_ebpf::debug;
use archon_monitor_common::{EventKind, SyscallEvent, PATH_LEN};

/// Single-entry map holding the cgroup ID of the build currently under
/// observation, written by userspace via [`aya::maps::Array`] before the
/// sandboxed process is moved into that cgroup. `0` means "no target set
/// yet" (cgroup ID 0 is never a real cgroup), so probes correctly no-op
/// before userspace finishes setup.
#[map]
static TARGET_CGROUP: Array<u64> = Array::with_max_entries(1, 0);

/// Events flow to userspace here. 256 KiB comfortably absorbs a burst of
/// `openat` calls (e.g. a `make -j` build) between userspace poll ticks.
#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

#[inline(always)]
fn current_tgid() -> u32 {
    (bpf_get_current_pid_tgid() >> 32) as u32
}

#[inline(always)]
fn in_tracked_cgroup() -> bool {
    let Some(&target) = TARGET_CGROUP.get(0) else { return false };
    if target == 0 {
        return false;
    }
    (unsafe { bpf_get_current_cgroup_id() }) == target
}

/// TEMPORARY diagnostic: logs cgroup-match state for `bwrap`-named
/// processes only (to avoid flooding the log with every process on the
/// system's openat calls). Remove once cgroup scoping is confirmed working
/// against a live kernel.
#[inline(always)]
fn debug_cgroup_match(ctx: &TracePointContext) {
    let comm = bpf_get_current_comm().unwrap_or([0; 16]);
    if &comm[..5] != b"bwrap" {
        return;
    }
    let target = TARGET_CGROUP.get(0).copied().unwrap_or(0);
    let current = unsafe { bpf_get_current_cgroup_id() };
    debug!(ctx, "bwrap openat: current_cgroup={} target_cgroup={}", current, target);
}

// --- syscalls:sys_enter_execve — flag unexpected tools ---

#[tracepoint]
pub fn archon_sys_enter_execve(ctx: TracePointContext) -> u32 {
    match try_execve(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_execve(ctx: TracePointContext) -> Result<u32, u32> {
    if !in_tracked_cgroup() {
        return Ok(0);
    }
    let tgid = current_tgid();
    // sys_enter_execve: filename (const char*) at offset 16.
    let filename_ptr: u64 = unsafe { ctx.read_at(16).map_err(|_| 0u32)? };
    emit_path_event(EventKind::Exec, tgid, filename_ptr as *const u8);
    Ok(0)
}

// --- syscalls:sys_enter_openat — flag writes outside srcdir/pkgdir ---
//
// Policy (which paths are "outside srcdir/pkgdir") is enforced in
// userspace against the resolved path, since that requires string
// comparison against the build's working directory that's awkward inside
// the verifier's constraints. The kernel side just reports every open the
// tracked tree performs; archon-monitor's policy engine does the judging.

#[tracepoint]
pub fn archon_sys_enter_openat(ctx: TracePointContext) -> u32 {
    match try_openat(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_openat(ctx: TracePointContext) -> Result<u32, u32> {
    debug_cgroup_match(&ctx);
    if !in_tracked_cgroup() {
        return Ok(0);
    }
    let tgid = current_tgid();
    // sys_enter_openat: dfd at 16, filename (const char*) at 24, flags at 32.
    let filename_ptr: u64 = unsafe { ctx.read_at(24).map_err(|_| 0u32)? };
    let flags: u64 = unsafe { ctx.read_at(32).map_err(|_| 0u32)? };
    with_reserved_event(EventKind::Open, tgid, |event| {
        event.open_flags = flags as u32;
        if let Ok(bytes) =
            unsafe { bpf_probe_read_user_str_bytes(filename_ptr as *const u8, &mut event.path) }
        {
            event.path_len = bytes.len().min(PATH_LEN) as u16;
        }
    });
    Ok(0)
}

// --- syscalls:sys_enter_connect — flag non-declared network egress ---

#[tracepoint]
pub fn archon_sys_enter_connect(ctx: TracePointContext) -> u32 {
    match try_connect(ctx) {
        Ok(ret) | Err(ret) => ret,
    }
}

fn try_connect(ctx: TracePointContext) -> Result<u32, u32> {
    if !in_tracked_cgroup() {
        return Ok(0);
    }
    let tgid = current_tgid();
    // sys_enter_connect: uservaddr (struct sockaddr*) at offset 24.
    let sockaddr_ptr: u64 = unsafe { ctx.read_at(24).map_err(|_| 0u32)? };
    if sockaddr_ptr == 0 {
        return Ok(0);
    }
    // struct sockaddr_in: sa_family (u16) at 0, port (u16, network order)
    // at 2, addr (u32, network order) at 4. Non-AF_INET sockaddrs are
    // skipped — Phase 3 scope is IPv4 TCP/UDP source verification.
    const AF_INET: u16 = 2;
    let family: u16 = unsafe {
        bpf_probe_read_user(sockaddr_ptr as *const u16).map_err(|_| 0u32)?
    };
    if family != AF_INET {
        return Ok(0);
    }
    let port_be: u16 = unsafe {
        bpf_probe_read_user((sockaddr_ptr + 2) as *const u16).map_err(|_| 0u32)?
    };
    let addr_be: u32 = unsafe {
        bpf_probe_read_user((sockaddr_ptr + 4) as *const u32).map_err(|_| 0u32)?
    };

    with_reserved_event(EventKind::Connect, tgid, |event| {
        event.addr_v4 = addr_be;
        event.port = u16::from_be(port_be);
    });
    Ok(0)
}

/// Shared tail for Exec/Open: read the user-space path string directly
/// into the reserved ring buffer slot.
fn emit_path_event(kind: EventKind, tgid: u32, path_ptr: *const u8) {
    with_reserved_event(kind, tgid, |event| {
        if let Ok(bytes) = unsafe { bpf_probe_read_user_str_bytes(path_ptr, &mut event.path) } {
            event.path_len = bytes.len().min(PATH_LEN) as u16;
        }
    });
}

/// Reserves a ring buffer slot, zero-initializes it (a cheap constant
/// write — not a stack copy of the whole event) and fills common fields,
/// then hands the caller a direct `&mut SyscallEvent` pointing into ring
/// buffer memory for kind-specific fields. This keeps the ~280-byte
/// `SyscallEvent` (dominated by the 256-byte path buffer) off the eBPF
/// stack entirely, which is capped at 512 bytes per frame by the verifier.
fn with_reserved_event(kind: EventKind, tgid: u32, fill: impl FnOnce(&mut SyscallEvent)) {
    let Some(mut entry) = EVENTS.reserve::<SyscallEvent>(0) else {
        // Ring buffer full: drop the event rather than block the build.
        // Phase 3's report surfaces a dropped-event count so this is
        // visible, not silently lossy.
        return;
    };
    let event = entry.write(SyscallEvent::zeroed(kind));
    event.pid = tgid;
    event.timestamp_ns = unsafe { bpf_ktime_get_ns() };
    if let Ok(comm) = bpf_get_current_comm() {
        event.comm = comm;
    }
    fill(event);
    entry.submit(0);
}

// The kernel's BPF loader reads this ELF section to decide whether
// GPL-only helpers (several of the tracing helpers used above are)
// may be called. This is a raw kernel-ABI requirement, not an aya API.
#[no_mangle]
#[link_section = "license"]
pub static LICENSE: [u8; 4] = *b"GPL\0";

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
