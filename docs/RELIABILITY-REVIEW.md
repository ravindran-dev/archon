# Reliability review — 2026-10-31

This review covers the installation, source/build, cache and monitor-storage
paths on the current Arch host. It supersedes the older descriptions of these
paths in historical audit evidence. It is not a production certification.

## Changes

| Finding | Implemented behavior |
|---|---|
| Every installation performed a full system upgrade | Normal installs use local databases. Already-current requests return without sudo or confirmation. Only explicit `install --refresh` runs `pacman -Syu`. |
| Refresh could be blocked by resolution against stale databases | Explicit refresh completes before resolving and displaying the fresh installation plan. Cancellation before refresh changes nothing; cancellation afterward states that the upgrade already completed. |
| Planner used hardcoded repo order and included disabled on-disk DBs | Production install/search reads `pacman-conf --repo-list`, preserving configured order and excluding disabled repositories. |
| Same-size/subsecond updates could retain stale caches | Fingerprints include nanosecond mtime, inode, size and ctime. Old cache schemas miss and regenerate. |
| Shared `.tmp` path raced concurrent writers and followed planted symlinks | Private unique temporary files are atomically published; failed writes remain best-effort cache misses. |
| No coordination between builds using the same source tree | Advisory directory-descriptor locks reject competing Archon builds and release automatically on exit. |
| Cached AUR sources never refreshed | Git fetch and fast-forward update before building. Tracked edits and diverged local commits are preserved and cause an actionable error. Git progress stays visible. Git configuration, hooks and filters execute inside the sandbox. |
| Current AUR packages were repeatedly built | Installed versions satisfying the plan are skipped; VCS package bases remain eligible for rebuilding. |
| Old archives could be selected by name alone | Regular package versions must match the plan. VCS outputs must be at least the planned version. Older cached outputs are ignored; matching ambiguous outputs are refused. |
| Missing AUR prerequisites were detected after official installation | Pending AUR builds check bubblewrap and the administrator-installed receiver before official dependencies are installed. |
| An inherited stdout/stderr descriptor could hang capture after a child exited | Polling readers stop after the parent exits, drain a bounded remainder, and report capture errors. Child guards kill/reap processes when orchestration exits early. |
| Root recursively changed ownership of user-controlled trees | No recursive chown remains. Monitored directory creation drops groups/GID/UID before exec; Git uses the same sandbox/build identity as source preparation. Build trees must already be writable by the build user. |
| Root opened SQLite inside user-writable build/cache paths | Monitor events use `/var/lib/archon/monitor.db` under validated administrator-owned state. Root metadata loads avoid persistent user caches. Root archive snapshots require build-user file ownership. |
| SQLite committed every observed syscall separately | Ring-buffer drains are persisted in batches with cached insert statements and foreign-key enforcement. |
| Inconclusive/flagged local monitored builds returned success | They now return an error as well as printing the report. |
| Several security regression scripts referenced obsolete APIs/dependencies | Probes use the production modules and locked workspace; AUR and snapshot regressions are included in CI. |

`--refresh` intentionally authorizes a full system upgrade even if later
resolution, prerequisite checks or building fails. Default installation never
implicitly authorizes that upgrade and never uses `pacman -Sy` alone.

## Local verification

- Locked workspace suite: 282 tests passed, including real unprivileged benign
  sandbox building on this host; no tests ignored in the local full run.
- Strict workspace/all-target Clippy, rustfmt and diff whitespace checks pass.
- Fourteen production-module AUR fixtures pass: success/failure propagation,
  dependency ordering, split outputs, immutable archive replacement defense,
  signatures, missing/ambiguous output and stale version selection. Fixture
  installers do not perform privileged package transactions.
- Real Linux snapshot seals reject writes, growth and shrinkage; symlink/FIFO
  archive inputs are rejected. Native `pacman -Qp` reads the immutable snapshot
  after replacement of the original package file.
- Real benign and malicious-demo builds completed inside bubblewrap in
  disposable directories. Host marker files were unchanged. These tests did
  not enable eBPF or install the resulting packages.
- The PGP symlink probe leaves its outside fixture unchanged and never calls
  host GPG. Thirteen Fedora bridge fixture tests and four release-evidence gate
  tests pass.
- The QEMU installed-state check returned without sudo or upgrade, measuring
  83–117 ms across warm runs (320 ms with a fresh parsed-metadata cache).
- The supplied event-store benchmark measured 1,000 individual commits at
  18.75 ms versus 1.27 ms in ten batches on this host. This compares the two
  persistence strategies, not complete eBPF throughput or another filesystem.

Reproduce core checks:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo build --release --locked --bins
ARCHON_AUDIT_DEPS="$PWD/target/release/deps" python3 scripts/repro-aur-install.py --direct
ARCHON_AUDIT_DEPS="$PWD/target/release/deps" python3 scripts/check-archive-snapshot.py
ARCHON_AUDIT_BIN="$PWD/target/release/archon" python3 scripts/check-live-sandbox.py
cargo run --release --locked -p archon-monitor --example bench_store
```

## Remaining release limits

Privileged monitoring/UID dropping and an actual receiver-to-pacman install
were not exercised in this review. A successful source/unit check is not
proof of those trust boundaries on a real kernel.

The eBPF report still does not establish complete observation: ring-buffer
loss accounting, broader syscall coverage and kernel/object compatibility
need further implementation and VM validation. The sandbox is the isolation
boundary; a clean report is not a guarantee of package safety.

The Arch plan remains advisory rather than a prepared libalpm transaction.
Custom DBPath/RootDir and per-repository Usage policies, complete installed
conflict/replacement modeling, AUR recipe/RPC metadata changes, durable recovery
across official/AUR transactions, and CPU/memory/disk build budgets remain
larger follow-up work. Makepkg/pacman perform their own final checks, and
failures stop installation rather than bypassing those checks.

This pass does not verify real native transactions on every advertised distro,
boot/reboot recovery, or live signatures against all system keyrings. The
existing production release evidence gate remains required.
