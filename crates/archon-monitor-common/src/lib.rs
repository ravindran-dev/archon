//! Event structs shared between the eBPF programs (`archon-monitor-ebpf`,
//! compiled `no_std` for the `bpfel-unknown-none` target) and the userspace
//! monitor (`archon-monitor`). Kept dependency-free and `#![no_std]` so it
//! compiles cleanly on both sides of that boundary.
//!
//! Events are POD (`#[repr(C)]`, `Copy`, no pointers/padding surprises) so
//! the eBPF side can write them directly into a `RingBuf` and userspace can
//! read them back with a raw pointer cast — see [`SyscallEvent::from_bytes`].

#![cfg_attr(not(test), no_std)]

pub const COMM_LEN: usize = 16;
pub const PATH_LEN: usize = 256;

/// Which syscall family a [`SyscallEvent`] reports. Kept as a plain `u8`
/// (not a Rust `enum` embedded in the struct) so the representation is
/// unambiguous across the eBPF/userspace ABI boundary.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Exec = 0,
    Open = 1,
    Connect = 2,
}

impl EventKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Exec),
            1 => Some(Self::Open),
            2 => Some(Self::Connect),
            _ => None,
        }
    }
}

/// One observed syscall, scoped to a tracked build's process tree.
///
/// `path` holds the exec'd binary's path (Exec), the file being opened
/// (Open), or is unused (Connect, which uses `addr_v4`/`port` instead).
/// Fixed-size buffers only — eBPF stack frames are capped at 512 bytes by
/// the verifier, and BTF-less user pointers must be read with a bounded
/// `bpf_probe_read_user_str_bytes` anyway.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SyscallEvent {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub pid: u32,
    pub timestamp_ns: u64,
    pub comm: [u8; COMM_LEN],
    pub path: [u8; PATH_LEN],
    pub path_len: u16,
    /// `openat`'s `flags` argument (raw, as passed to the syscall — see
    /// `open(2)`'s `O_*` constants). Unused for Exec/Connect. Carried so
    /// the policy engine can tell a read from a write attempt instead of
    /// flagging every read of system headers/libraries during a build.
    pub open_flags: u32,
    /// IPv4 address in network byte order; 0 if unset or not IPv4.
    pub addr_v4: u32,
    /// Port in host byte order; 0 if unset.
    pub port: u16,
}

/// Linux `open(2)` flag bits relevant to write-intent detection. Values are
/// the generic Linux/x86_64 `O_*` constants — stable ABI, not looked up at
/// runtime.
pub mod open_flags {
    pub const O_ACCMODE: u32 = 0o0000003;
    pub const O_WRONLY: u32 = 0o0000001;
    pub const O_RDWR: u32 = 0o0000002;
    pub const O_CREAT: u32 = 0o0000100;
    pub const O_TRUNC: u32 = 0o0001000;
}

impl SyscallEvent {
    /// Whether `open_flags` requests write access or file creation —
    /// the cases the policy engine cares about for "$srcdir/$pkgdir only".
    /// Read-only opens of system headers/libraries during a build are
    /// normal and must not be flagged.
    pub fn is_write_open(&self) -> bool {
        use open_flags::*;
        let mode = self.open_flags & O_ACCMODE;
        mode == O_WRONLY || mode == O_RDWR || self.open_flags & (O_CREAT | O_TRUNC) != 0
    }

    pub const fn zeroed(kind: EventKind) -> Self {
        SyscallEvent {
            kind: kind as u8,
            _pad: [0; 3],
            pid: 0,
            timestamp_ns: 0,
            comm: [0; COMM_LEN],
            path: [0; PATH_LEN],
            path_len: 0,
            open_flags: 0,
            addr_v4: 0,
            port: 0,
        }
    }

    /// The exec/open path as a `&str`, lossily if it somehow isn't valid
    /// UTF-8 (filenames are arbitrary bytes on Linux; we favor never
    /// panicking in the report path over strict correctness here).
    pub fn path_str(&self) -> &str {
        let len = (self.path_len as usize).min(PATH_LEN);
        core::str::from_utf8(&self.path[..len]).unwrap_or("")
    }

    pub fn comm_str(&self) -> &str {
        let len = self.comm.iter().position(|&b| b == 0).unwrap_or(COMM_LEN);
        core::str::from_utf8(&self.comm[..len]).unwrap_or("")
    }

    /// # Safety
    /// `bytes` must be exactly `size_of::<SyscallEvent>()` bytes produced
    /// by writing a `SyscallEvent` (e.g. via the ring buffer), so its bit
    /// pattern is a valid `SyscallEvent` — no live pointers or invalid enum
    /// discriminants beyond what `kind` already tolerates via `from_u8`.
    pub unsafe fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != core::mem::size_of::<SyscallEvent>() {
            return None;
        }
        Some(unsafe { (bytes.as_ptr() as *const SyscallEvent).read_unaligned() })
    }

    pub fn as_bytes(&self) -> &[u8] {
        let ptr = self as *const Self as *const u8;
        unsafe { core::slice::from_raw_parts(ptr, core::mem::size_of::<Self>()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_bytes() {
        let mut ev = SyscallEvent::zeroed(EventKind::Exec);
        ev.pid = 4242;
        ev.timestamp_ns = 123456789;
        let path = b"/usr/bin/gcc";
        ev.path[..path.len()].copy_from_slice(path);
        ev.path_len = path.len() as u16;

        let bytes = ev.as_bytes().to_vec();
        let back = unsafe { SyscallEvent::from_bytes(&bytes) }.unwrap();
        assert_eq!(back.pid, 4242);
        assert_eq!(back.timestamp_ns, 123456789);
        assert_eq!(back.path_str(), "/usr/bin/gcc");
        assert_eq!(EventKind::from_u8(back.kind), Some(EventKind::Exec));
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(unsafe { SyscallEvent::from_bytes(&[0u8; 4]) }.is_none());
    }

    #[test]
    fn write_open_detection() {
        use open_flags::*;
        let mut ev = SyscallEvent::zeroed(EventKind::Open);
        ev.open_flags = 0; // O_RDONLY
        assert!(!ev.is_write_open());
        ev.open_flags = O_WRONLY;
        assert!(ev.is_write_open());
        ev.open_flags = O_RDWR;
        assert!(ev.is_write_open());
        ev.open_flags = O_CREAT; // O_RDONLY|O_CREAT: creating a new file
        assert!(ev.is_write_open());
    }

    #[test]
    fn comm_str_stops_at_nul() {
        let mut ev = SyscallEvent::zeroed(EventKind::Open);
        ev.comm[..3].copy_from_slice(b"gcc");
        assert_eq!(ev.comm_str(), "gcc");
    }
}
