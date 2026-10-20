# Archon audit — current disposition

Updated 2026-10-20. Baseline c09eb15; implementation through 1575c6e; earlier verification tooling/evidence bf7b12e.
Commit author supplied by the user: ravindran-dev <ravindrans.dev@gmail.com>.
This is a scoped audit with confirmed fixes, not certification of a perfect
package manager. No host package installation/removal or privileged command
was run. A later user-authorized attempt confirmed host elevation is blocked:
`sudo -n true` exits 1 because `/etc/sudo.conf` belongs to uid 65534 and
`no new privileges` blocks sudo; `/usr/bin/sudo` itself also belongs to uid
65534. Docker, Podman and nerdctl are absent, cgroup v2 is not writable, and
syscall tracepoint format access is denied. No host ownership, sudo, cgroup,
or package state was changed. Exact output:
[authorized attempt](audit/evidence/production/authorized-attempt.txt).
The actual host is Arch Linux, uid 1000; the original prompt's
Ubuntu environment description does not match this workspace.

The current disposition below supersedes the chronological Phase 0 notes
retained at the end. Their old blocked/fixed statuses describe earlier runs.
Original code authorship and prior README/live-testing claims were not
independently established. The README demo is now identified as historical.

## Release-blocker follow-up (latest, supersedes the rows below)

PGP traversal is fixed in source (2baa64c): the privileged parent no longer creates,
chmods, chowns or runs GPG on the package keyring. Automatic imports use the
existing bwrap filesystem policy and, on the monitored path, the same non-root
build identity. Root build identities are rejected before spawning. Invalid
fingerprints are excluded; GPG receives --no-options and explicit argument
separators. Failed imports warn and leave makepkg signature verification active.

Failed-first command: `python3 scripts/check-pgp-symlink.py`.
Raw before: outside fixture mode 0755 -> 0700, exit 1.
Raw after: mode 0755 -> 0755, no host GPG invocation, exit 0.
[Before](audit/evidence/release-blockers/pgp-before.txt),
[after](audit/evidence/release-blockers/pgp-after.txt),
[sandbox/invalid-key regression](audit/evidence/release-blockers/pgp-sandbox-after.txt).
The sandbox crate's 14 actual unit tests pass, including rejecting uid 0.
Clippy/fmt pass; full workspace tests still cannot download fastrand v2.4.1.
The matrix was rerun: all six UNVERIFIED because Docker is absent.
Raw logs are under audit/evidence/release-blockers/pgp-*.txt and sandbox-tests.txt.
Live bwrap isolation and privileged uid/gid dropping remain UNVERIFIED here;
the mock regression proves dispatch and absence of host keyring mutations,
not a functioning kernel namespace boundary.

### Immutable archive handoff

The check-to-install race is fixed in source (1575c6e). `archive::PackageArchive` opens
regular files with O_NOFOLLOW/O_NONBLOCK, copies them to a Linux memfd, and
adds WRITE/GROW/SHRINK/SEAL seals before any identity check. Both `pacman -Qp`
and the eventual `pacman -U` read the same `/proc/<archon-pid>/fd/<n>` snapshot;
the parent retains it until pacman exits. Neither installation path reopens
the mutable build output. Failed sealing/copying is an error; no mutable fallback.

Failed-first reproduction: `python3 scripts/repro-aur-install.py --direct archive-race`.
Raw before: `installed fixture payloads: [[{'name': 'attacker-replacement'}]]`,
then `AssertionError: validated archive was replaced before installation`.
Raw after: `installed fixture payloads: [[{'name': 'fixture-app'}]]`,
`PASS: archive-race`. The fake identity query replaces the original archive
before the fake installation reads it. No host package transaction is executed.
[Before](audit/evidence/release-blockers/archive-race-before.txt),
[after, all 12 scenarios](audit/evidence/release-blockers/archive-install-after.txt).

Additional native read-only probe: `python3 scripts/check-archive-snapshot.py`.
Raw output:

```text
PASS: original replacement, write, grow, shrink, symlink and FIFO attacks rejected
fixture-app 1-1
PASS: real pacman parses sealed snapshot after original archive replacement
Snapshot source label: fixture-app-1-1-any.pkg.tar
```

[Complete native output](audit/evidence/release-blockers/archive-snapshot-after.txt).
Native pacman metadata parsing and kernel seals were actually exercised;
privileged pacman installation and cross-user procfs permissions are UNVERIFIED.
If procfs or memfd access is restricted, installation fails rather than falling
back to a writable path. Snapshots consume temporary memory/swap proportional
to the package outputs being installed in that build-base transaction.
This protects the validation-to-install handoff, not the integrity of a build
output *before* it is snapshotted or all privileged handling of the build tree.

Descriptor paths cannot expose an adjacent `.sig`. To avoid silently dropping
an existing detached signature under Optional policy, signature bytes are also
snapshotted and `pacman-key --verify` must succeed against the frozen package
before identity checking. Missing verifier, invalid/untrusted signatures and
signature symlinks fail closed. The valid/invalid verification paths use mocks;
live cryptographic verification against the host system keyring is UNVERIFIED.
The signature regression failed first when verification was absent:
[before](audit/evidence/release-blockers/snapshot-signature-before.txt).

The former descriptor handoff could not preserve a sibling signature under
`LocalFileSigLevel=Required`; the regression failed first:
[before](audit/evidence/release-blockers/snapshot-policy-before.txt). The
selected replacement is a fixed-path root-owned receiver. It accepts only
bounded archive/signature bytes, stages them under `/var/lib/archon` with
private permissions, and invokes `/usr/bin/pacman -U --needed --` with the
original system configuration. It does not execute package data or change
pacman policy. `archon setup-helper` performs a one-time explicit sudo copy;
that privileged setup has not been run on this host. Hermetic protocol tests
exercise signed staging, malformed/truncated inputs, traversal, cleanup, and
permissions. Native required-policy verification and the actual helper-to-
pacman transaction remain UNVERIFIED pending an Arch integration run.

All direct install fixtures pass, including signed transaction staging,
signature-required dispatch, native verifier failure propagation, archive
replacement, split outputs, and dependency ordering. The native verifier is
mocked in these tests; they do not prove actual pacman PGP behavior.

`scripts/test-security-offline.sh` runs the actual PGP/archive module tests:
6 passed, 0 failed, 0 ignored. It uses a separate temporary manifest with cached
compatible tempfile 3.24.0/fastrand 2.3.0, not the workspace's locked versions.
It does not change Cargo.lock or establish full-workspace verification.
[Raw unit output](audit/evidence/release-blockers/security-module-tests.txt).
Clippy/fmt pass after the archive fix; locked workspace tests remain blocked
by missing fastrand v2.4.1. Matrix rerun: six UNVERIFIED (Docker absent).
[Clippy](audit/evidence/release-blockers/archive-clippy.txt),
[fmt](audit/evidence/release-blockers/archive-fmt.txt),
[workspace failure](audit/evidence/release-blockers/archive-workspace-tests.txt),
[matrix](audit/evidence/release-blockers/archive-matrix.txt).

### Final follow-up results and environment blockers

The optimized binary containing both fixes is deployed to
`/home/ravi/.local/bin/archon`, still version 0.2.0. Current SHA256:
`e442ba29272a900a2f282a1fe2bc22c9edc6446d2b2e68779cb34021122ef94c`.
Backup: `/tmp/archon-before-security-fixes-htp443ox`.
[Release build](audit/evidence/release-blockers/release-build.txt),
[deployment](audit/evidence/release-blockers/installed-binary.txt),
[installed CLI dry-run](audit/evidence/release-blockers/installed-dry-run.txt).

Final build, strict clippy and formatting pass. There are 148 passing targeted
Rust unit tests: types 29, resolver 40, distro 27, sandbox 14, monitor-common 4,
monitor 17, AUR model 4, correction 2, srcinfo 5, PGP 4 and archive 2. The last
six use the explicitly documented temporary security-test workspace. All nine
AUR tests were also attempted: four pass, five fail at socket creation/bind
with Operation not permitted; zero are ignored. No full-workspace success is
claimed. [Raw final checks](audit/evidence/release-blockers/final-workspace-tests.txt),
[clippy](audit/evidence/release-blockers/final-clippy.txt),
[format](audit/evidence/release-blockers/final-fmt.txt),
[AUR failures](audit/evidence/release-blockers/aur-all-tests.txt).

The actual unprivileged bwrap/GPG preparation path was additionally exercised
without network: an intentionally invalid bundled public-key file reaches real
GPG, and a deliberately invalid package gpg.conf is ignored by --no-options.
GPG rejects the invalid data and Archon warns that makepkg signature verification
is still required. This verifies isolation dispatch/config suppression, not a
successful signature verification:
[raw live GPG test](audit/evidence/release-blockers/pgp-sandbox-live.txt).

A minimal real bwrap mount/user/PID namespace command succeeds. A real benign
demo build succeeds at Fetch but FAILS at Build with:

```text
bwrap: loopback: Failed to create NETLINK_ROUTE socket: Operation not permitted
```

[Raw build failure](audit/evidence/release-blockers/live-sandbox-builds.txt),
[minimal namespace check](audit/evidence/release-blockers/bwrap-live.txt).
The suite stops there; the malicious demo is not reached. No network isolation
was removed to obtain a passing build. Full build isolation, malicious-demo
flagging, root uid/gid switching and real privileged installation stay UNVERIFIED.
The test only builds temporary copies; it never installs an archive.

The remaining release checks cannot be completed in this environment:

| Release check | Latest result / raw evidence |
|---|---|
| Full locked workspace tests | UNVERIFIED: missing fastrand 2.4.1; online fetch also fails for tempfile 3.27.0 because static.crates.io cannot resolve. [Fetch attempt](audit/evidence/release-blockers/fetch.txt) |
| Six real distro smoke containers | All six UNVERIFIED: Docker/Podman absent. [Table](audit/evidence/release-blockers/archive-matrix.txt) |
| Six native installation/removal containers | All six UNVERIFIED: Docker absent. [Table](audit/evidence/release-blockers/matrix-install.txt) |
| Live eBPF / tracepoint verification | UNVERIFIED: tracefs permission denied, uid 1000, cgroup root not writable. [Kernel checks](audit/evidence/release-blockers/tracepoints.txt), [environment](audit/evidence/release-blockers/environment.txt) |
| Live privileged pacman handoff | UNVERIFIED: no privileged transaction authorized/run in this audit; requires an appropriate disposable Arch environment |

No host packages were installed/removed and no sudo/root command was executed.
No lockfile downgrade, test deletion, ignored failure, isolation relaxation,
or fake container/eBPF success was used. These are still release-verification
blockers; the two confirmed source vulnerabilities no longer remain unfixed.

## Current findings

Locations refer to current source unless explicitly marked baseline.
Each fixed defect has failed-first evidence in the linked files and a separate
fix commit. Coverage is restricted to the cases actually exercised.

| Severity / area | Location | Root cause and case | Status / commit |
|---|---|---|---|
| High / A | archon-distro/src/lib.rs:56 | ID and ID_LIKE merged in fixed family order; Ubuntu + ID_LIKE=arch selected Arch | fixed / 4919b3e |
| High / D | archon-types/src/version.rs:108 | Mixed ALPM/Dpkg comparison panicked instead of returning a typed error | fixed / 2885b19 |
| High / D | archon-resolver/src/search.rs:236 | Greedy selection retained rejected branches or failed after late conflicts; solvable alternatives/providers rejected | fixed, bounded backtracking / 827c70c |
| High / D | archon-resolver/src/search.rs:67 | Only one already-selected virtual binding was tried, producing a false AUR cycle | fixed / 41a9598 |
| High / D | archon-types/src/version.rs, rpmvercmp | Debian tilde semantics used for ALPM: 110 mismatches against native vercmp in 900 pairs | fixed / 5e034d9 |
| High / C | archon-aur/src/model.rs:58 | RPC CheckDepends dropped despite default makepkg checks | fixed / 5a94820 |
| High / C,D | archon-aur/src/model.rs:77; archon-resolver/src/lib.rs:215 | PackageBase lost; split siblings built separately and same-base runtime cycles rejected | fixed / d80b4ca |
| High / G | archon/src/commands/install.rs:955 | Filename prefix accepted another split output (fixture-app-addon for fixture-app) | fixed, native archive identity check / 101a818 |
| High / E | archon/src/correction.rs:3; commands/install.rs, typo path | Byte slicing panicked on éé; Arch suggestions consulted AptLists | fixed / 75b4c84 |
| High / F | archon-monitor/src/report.rs, is_clean; commands/install.rs:812 | Zero captured events authorized a clean report; flagged/inconclusive installs needed explicit failure | fixed in report/CLI logic / ea0a3ce; live enforcement UNVERIFIED |
| High / H,F | archon/src/pgp.rs:64,99; commands/install.rs:821 | Package-controlled .gnupg symlink is followed and its target chmodded; monitored caller runs helper as root before build privilege drop | fixed / 2baa64c: no host keyring writes or GPG subprocess; live sandbox/drop verification UNVERIFIED |
| High / H | archon/src/commands/install.rs, archive selection/install | User-writable archive may change between pacman -Qp and privileged pacman -U | fixed in source: sealed immutable snapshot shared by validation and installation; real privileged transaction UNVERIFIED |
| Medium / D | archon-resolver/src/lib.rs:215 | Chosen AUR alternative edges absent from build order | fixed / bb5bad9 |
| Medium / D | archon-resolver/src/search.rs:187 | Incompatible virtual providers accepted; dependencies of conflict-rejected candidates produced misleading NotFound | fixed / 49c11c2, 3cbb779 |
| Medium / F | archon/src/srcinfo.rs | git+https source hosts omitted from monitor allowlist | fixed for tested transports / 7c3f1bf |
| Medium / G | archon/src/commands/install.rs, AUR dispatch | install only built output and exited successfully without installation | fixed, build/install per PackageBase in dependency order / 2445ace; real privileged transaction UNVERIFIED |
| Medium / E,D | archon-resolver/src/suggest.rs:19 | Equal suggestion scores depended on index order | fixed / 417a0f4 |
| Medium / G | archon-types/src/bin/verify_dpkg.rs | Comparator checker printed mismatches but exited successfully | fixed / e1d9f2b; actual native dpkg parity UNVERIFIED |
| Medium / A | archon/src/commands/build.rs:25; archon-sandbox/src/runner.rs | Local build hardcodes makepkg and lacks backend/tool preflight | needs-decision: explicit non-Arch refusal vs managed Arch container; non-Arch runtime UNVERIFIED |
| Medium / A | archon/src/commands/install.rs:175 | Non-Arch installation delegates; AptLists is not used in install resolution | needs-decision: retain delegation vs integrate resolver; existing delegation retained |
| Medium / A | archon-distro/src/lib.rs:45 | Unreadable override-path diagnostic names /etc/os-release | unfixed-with-reason: original user's existing-file symptom not reproduced; diagnostic remains misleading |
| Medium / B | archon-repo/src/lib.rs; commands/install.rs | Fixed official repo order; pacman.conf/custom Include/architecture handling not exhaustively audited | UNVERIFIED |
| High / F | archon-monitor-ebpf/src/main.rs | Fixed tracepoint offsets and prebuilt object/source compatibility | UNVERIFIED: tracefs permission denied; no privileged access requested or used |
| Low / G | workspace | Existing strict clippy and formatting failures | fixed / c01b9bd |
| Low / E | archon/tests/cli.rs; tests/common/mod.rs | Depth-cap test had no assertions | replaced with chained RPC regression / dc52eb1; full CLI test UNVERIFIED due dependency/socket restrictions |

Paths above are under crates/. No claim is made that every requested A–H
subcase has been completed. Specifically, RPC batching/rate limits/timeouts,
cache atomicity/concurrency, all pacman group/replaces semantics, all input
injection surfaces, Ctrl-C cleanup, IPv6/UNIX socket policy, relative openat
dirfd handling, interpreter execution and ring-buffer drop accounting still
need dedicated reproduction and verification. No global panic-site signoff
or arbitrary security-package compatibility claim is made.

## Final verification

The raw logs include commands, stdout/stderr and exit status. No failing test
was removed or ignored. The cached-crate harness compiles actual source/test
modules against existing dependency rlibs; it is not a full locked-workspace
test substitute. Explicit filtered runs below do not hide the full AUR result.

| Check | Result | Raw evidence |
|---|---|---|
| cargo build --workspace --locked --offline | PASS | [build](audit/evidence/final-build.txt) |
| cargo clippy --workspace --locked --offline -- -D warnings | PASS | [clippy](audit/evidence/final-clippy.txt) |
| cargo fmt --check | PASS | [fmt](audit/evidence/final-fmt.txt) |
| cargo test --workspace --locked --offline | UNVERIFIED: cannot download fastrand v2.4.1 | [failure](audit/evidence/final-workspace-tests.txt), [network attempt](audit/evidence/completion-fetch.txt) |
| Actual types/resolver/distro/sandbox/monitor-common/monitor tests | PASS: 29 / 40 / 27 / 14 / 4 / 17 | latest *-tests.txt under audit/evidence/release-blockers |
| Actual AUR crate, all nine tests | 4 PASS, 5 FAIL: localhost bind Operation not permitted; 0 ignored | [full failure](audit/evidence/final-cached-aur-all.txt) |
| AUR model subset | PASS: 4, 5 explicitly filtered | [subset](audit/evidence/final-cached-aur-model.txt) |
| Correction/srcinfo std-only tests | PASS: 2 / 5 | [correction](audit/evidence/final-correction.txt), [srcinfo](audit/evidence/final-srcinfo.txt) |
| Production AUR installation function, fake bwrap/pacman/sudo | PASS: 12 scenarios, no privileged command executed | [raw](audit/evidence/release-blockers/archive-install-after.txt) |
| Production typo path | PASS: Unicode + backend isolation | [raw](audit/evidence/final-typo-regressions.txt) |
| Native ALPM comparator parity | PASS: 900 pairs, 0 mismatches | [raw](audit/evidence/final-version-parity.txt) |
| Native Dpkg comparator parity | UNVERIFIED: dpkg absent | [raw](audit/evidence/final-version-parity.txt) |
| Deliberately incorrect dpkg mock must fail checker | PASS: mismatches exit 1 | [raw](audit/evidence/final-dpkg-checker.txt) |
| Tracepoint format parser | PASS: 4 tests; kernel compatibility not established | [raw](audit/evidence/final-tracepoint-parser.txt) |
| Real kernel tracepoint formats | UNVERIFIED: permission denied | [raw](audit/evidence/final-tracepoints.txt) |
| Shell script syntax | PASS; Dockerfile/runtime still UNVERIFIED | [raw](audit/evidence/final-script-syntax.txt) |

The latest targeted runs cover 148 passing Rust unit tests, including the four
AUR model tests and six security-module tests documented in the follow-up. Five additional AUR client tests fail for the logged environment
restriction. Full workspace, repo and CLI integration suites remain UNVERIFIED.
The follow-up above exercises partial live bwrap/GPG behavior, but full builds
are blocked by NETLINK_ROUTE permissions. Real AUR downloads/builds, evil-demo
detection, root install success and Ctrl-C cleanup remain UNVERIFIED.

## Container matrix and privilege requirements

Run `scripts/matrix.sh` for identical smoke scenarios and `--install` for
native jq installation/removal inside disposable containers. The script builds
a static executable from this worktree in a Docker build stage, then tests
real os-release detection, dry-run dispatch, fixture precedence/unknown IDs,
and CLI entry points in each image. Debian-family images also run the native
dpkg parity checker. Logs are per image. Native transactions run as a container
user with container-only sudo. No host filesystem mount or privileged flag is
used. Build bootstrap requires network and installs dependencies only inside
images; smoke runtime disables network and uses a read-only root filesystem.

| Image | Smoke | Native transaction |
|---|---|---|
| archlinux:latest | UNVERIFIED: Docker absent | UNVERIFIED: Docker absent |
| ubuntu:24.04 | UNVERIFIED: Docker absent | UNVERIFIED: Docker absent |
| debian:stable | UNVERIFIED: Docker absent | UNVERIFIED: Docker absent |
| fedora:latest | UNVERIFIED: Docker absent | UNVERIFIED: Docker absent |
| opensuse/tumbleweed | UNVERIFIED: Docker absent | UNVERIFIED: Docker absent |
| alpine:latest | UNVERIFIED: Docker absent | UNVERIFIED: Docker absent |

Raw tables: [smoke](audit/evidence/final-matrix-smoke.txt),
[install](audit/evidence/final-matrix-install.txt). Exit 2 means unavailable,
not a pass. Ordinary detection, comparator and dry-run checks require no root.
Docker access is needed for matrix execution. bwrap requires usable user
namespaces or suitable sandbox capabilities; restricted containers may deny
these. eBPF loading and cgroup creation additionally require kernel support,
privileges, readable tracefs and writable/delegated cgroup v2 access. Those
checks are deliberately separate from this unprivileged smoke matrix; a
privileged container would need explicit authorization and host kernel/cgroup
access. No `--privileged` test was run.

## Earlier installed executable (superseded by follow-up deployment)

The optimized worktree binary was atomically installed at
`/home/ravi/.local/bin/archon`; no distro package transaction was performed.
The previous executable is backed up at `/tmp/archon-before-final-2n9s2dpg`.
Shell lookup and version return `/home/ravi/.local/bin/archon`, `archon 0.2.0`.
SHA256: `3dbc95afba446073b568b9876947e14b4aac5fb04236595b23257d181df5489a`.
[Raw deployment](audit/evidence/final-installed-binary.txt),
[installed CLI fixture dry-run](audit/evidence/final-installed-dry-run.txt).
The version string is unchanged; this is a local source build, not a published
release. The PGP finding is still present and documented; no root workflow
was invoked to test or deploy it.

## Real repository resolution timings

`scripts/bench-resolver.sh` reads host official sync/local DBs without writing,
downloading or installing. The probe is optimized but links debug-built
libraries, so these are local indicative timings, not release benchmarks.
Latest run loaded DBs in 9147.79 ms, resolved vlc (208 packages) in 75.98 ms,
mpv (257) in 72.46 ms, ffmpeg (241) in 61.92 ms, libreoffice-fresh (194) in
41.86 ms. Security-tool requests nmap, wireshark-qt, metasploit, sqlmap,
hashcat, john, hydra and aircrack-ng also resolved in this official-only run.
This does not verify their installation or runtime behavior. Google Chrome
is absent from official repositories and requires AUR; its AUR timing is
UNVERIFIED. [Complete raw timings](audit/evidence/final-benchmark.txt).

## PGP security issue: failed-first reproduction (now fixed)

Command (no root, network or real gpg; all changes within a temporary fixture):

```sh
python3 scripts/check-pgp-symlink.py
```

Raw output:

```text
Outside fixture directory mode BEFORE: 0755
Outside fixture directory mode AFTER:  0700
CONFIRMED: import_build_keys follows .gnupg symlink outside the package tree.
UNVERIFIED: root impact; this probe runs only as the current unprivileged user.
```

Exit 1. [Raw evidence](audit/evidence/final-pgp-symlink.txt). The helper uses
create_dir_all and set_permissions on the package-controlled path. The
monitored caller explicitly invokes it before dropping root privileges.
Untrusted fingerprints and gpg configuration paths also need review; their
exploitability has not been demonstrated. The source comment calling this
"cosmetic" is not evidence of safety. This historical reproduction is now fixed by 2baa64c, which preserves automatic
imports inside bwrap as the build user. See the release-blocker follow-up for
after-fix evidence and live-verification limits.

## Completion regressions: commands and raw before/after outputs

The full raw outputs are preserved under audit/evidence. The sections below
include unmodified output from the failed-first and passing runs. The temporary
minimal resolver workspace uses compatible cached dependencies, not the complete
workspace lockfile; its exact setup is scripts/test-resolver-offline.sh.

### Typed version error

```sh
scripts/test-resolver-offline.sh mixed_backend_comparison_returns_without_panicking
```

Before ([complete log](audit/evidence/completion-version-before.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.ZmDJ69/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.ZmDJ69/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.10s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.00s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 1 test

thread 'version::tests::mixed_backend_comparison_returns_without_panicking' (724) panicked at crates/archon-types/src/version.rs:93:13:
CRITICAL: Cross-backend version comparison attempted! 1 (Alpm) vs 1 (Dpkg). This indicates a resolver graph cross-contamination between distros.
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

thread 'version::tests::mixed_backend_comparison_returns_without_panicking' (724) panicked at crates/archon-types/src/version.rs:333:9:
assertion failed: std::panic::catch_unwind(|| arch.vercmp(&deb)).is_ok()
test version::tests::mixed_backend_comparison_returns_without_panicking ... FAILED

failures:

failures:
    version::tests::mixed_backend_comparison_returns_without_panicking

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p archon-types --lib`
```

After ([complete log](audit/evidence/completion-version-after.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.No7Ufj/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.No7Ufj/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.41s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 1 test
test tests::mixed_backend_resolution_returns_typed_error ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.00s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 1 test
test version::tests::mixed_backend_comparison_returns_without_panicking ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s

     Running unittests src/bin/verify_dpkg.rs (/tmp/archon-audit-resolver-target/debug/deps/verify_dpkg-6b538ebfda907eb1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### Branch-local backtracking

```sh
scripts/test-resolver-offline.sh backtrack
```

Before ([complete log](audit/evidence/completion-backtracking-before.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.QGeTvy/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.QGeTvy/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.18s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 2 tests

thread 'tests::backtracks_virtual_provider_after_late_conflict' (371) panicked at crates/archon-resolver/src/lib.rs:623:39:
called `Result::unwrap()` on an `Err` value: PackageConflict { a: "blocker", b: "a-provider", declared_by: "blocker" }
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

thread 'tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies' (370) panicked at crates/archon-resolver/src/lib.rs:610:39:
called `Result::unwrap()` on an `Err` value: PackageConflict { a: "blocker", b: "bad", declared_by: "blocker" }
test tests::backtracks_virtual_provider_after_late_conflict ... FAILED
test tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies ... FAILED

failures:

failures:
    tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies
    tests::backtracks_virtual_provider_after_late_conflict

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 29 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p archon-resolver --lib`
```

After ([complete log](audit/evidence/completion-backtracking-after.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.kWGyiB/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.kWGyiB/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.37s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 37 tests
test tests::aur_cycle_is_reported_with_path ... ok
test tests::alternatives_fallback_if_primary_fails ... ok
test suggest::tests::test_no_suggestions_for_wildly_different_strings ... ok
test tests::aur_alternative_is_built_before_its_dependent ... ok
test tests::aur_build_order_is_dependency_first ... ok
test tests::aur_package_named_target_does_not_create_spurious_edges ... ok
test suggest::tests::test_suggest_obvious_typos ... ok
test tests::all_virtual_providers_conflict_reports_conflict_before_dependencies ... ok
test tests::alternatives_fallback_under_conflict ... ok
test tests::backtracking_recovers_from_aur_cycle ... ok
test tests::aur_package_pulls_official_deps_and_makedeps ... ok
test tests::backtracking_recovers_from_missing_transitive_dependency ... ok
test tests::aur_makedepends_ordered_before_dependent ... ok
test tests::debian_target_constraints_use_dpkg_backend ... ok
test tests::declared_conflicts_are_fatal ... ok
test tests::backtracks_virtual_provider_after_late_conflict ... ok
test tests::backtracking_restores_already_selected_dependencies ... ok
test tests::mixed_backend_resolution_returns_typed_error ... ok
test tests::linear_chain_resolves_fully ... ok
test tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies ... ok
test tests::diamond_appears_once ... ok
test tests::not_found_names_the_dependent ... ok
test tests::official_cycles_are_tolerated ... ok
test tests::official_provides_beats_aur_exact_name_match ... ok
test tests::search_exhaustion_is_explicit ... ok
test tests::official_shadows_aur_same_name ... ok
test tests::targets_have_empty_required_by ... ok
test tests::provides_resolved_package_is_also_found_by_concrete_name_in_same_batch ... ok
test tests::prefer_installed_avoids_unnecessary_conflicting_swap ... ok
test tests::unsatisfiable_version_reports_whats_available ... ok
test tests::self_conflict_is_ignored ... ok
test tests::versioned_target_spec_is_honored ... ok
test tests::version_conflict_names_both_requirers ... ok
test tests::virtual_package_resolved_via_provides ... ok
test tests::versioned_provide_checked_against_constraint ... ok
test tests::virtual_provider_avoids_conflict_with_selected_package ... ok
test tests::virtual_provider_respects_reverse_conflict_even_when_installed ... ok

test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 28 tests
test dependency::tests::bare_provide_never_satisfies_versioned_dep ... ok
test dependency::tests::constraint_ops_match_vercmp ... ok
test dependency::tests::parses_ge_constraint ... ok
test dependency::tests::ge_is_not_gt_plus_eq ... ok
test dependency::tests::eq_constraint_without_pkgrel_matches_any_pkgrel ... ok
test dependency::tests::parses_unversioned ... ok
test dependency::tests::provide_name_must_match ... ok
test dependency::tests::strips_colon_description_suffix ... ok
test dependency::tests::unconstrained_dep_accepts_anything ... ok
test dependency::tests::versioned_provide_satisfies_versioned_dep ... ok
test package::tests::satisfies_by_exact_name_and_version ... ok
test package::tests::satisfies_via_provides ... ok
test version::tests::debian_tilde_sorts_older_than_anything ... ok
test version::tests::display_roundtrips ... ok
test version::tests::dpkg_version_comparisons ... ok
test version::tests::leading_zeros_are_insignificant ... ok
test version::tests::epoch_dominates_everything ... ok
test version::tests::man_page_alpha_chain ... ok
test version::tests::missing_pkgrel_matches_any_pkgrel ... ok
test version::tests::mixed_backend_comparison_returns_without_panicking ... ok
test version::tests::numeric_compares_by_value_not_lexically ... ok
test version::tests::numeric_segment_beats_alphabetic ... ok
test version::tests::man_page_numeric_chain ... ok
test version::tests::parses_epoch_ver_rel ... ok
test version::tests::pkgrel_breaks_ties ... ok
test version::tests::pkgrel_with_subrelease ... ok
test version::tests::parses_bare_version ... ok
test version::tests::separator_runs_compare_by_length ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/bin/verify_dpkg.rs (/tmp/archon-audit-resolver-target/debug/deps/verify_dpkg-6b538ebfda907eb1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_resolver

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_types

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### Already-selected virtual bindings

```sh
scripts/test-resolver-offline.sh backtracks_existing_virtual_binding_to_avoid_false_aur_cycle
```

Before ([complete log](audit/evidence/completion-existing-provider-before.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.hCrwZq/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.hCrwZq/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.97s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 1 test

thread 'tests::backtracks_existing_virtual_binding_to_avoid_false_aur_cycle' (371) panicked at crates/archon-resolver/src/lib.rs:662:52:
called `Result::unwrap()` on an `Err` value: DependencyCycle { cycle: ["a-app", "b-provider", "a-app"] }
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test tests::backtracks_existing_virtual_binding_to_avoid_false_aur_cycle ... FAILED

failures:

failures:
    tests::backtracks_existing_virtual_binding_to_avoid_false_aur_cycle

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 39 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p archon-resolver --lib`
```

After ([complete log](audit/evidence/completion-existing-provider-after.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.TzRiUL/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.TzRiUL/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.10s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 40 tests
test suggest::tests::suggestion_ties_do_not_depend_on_index_iteration_order ... ok
test tests::aur_build_order_is_dependency_first ... ok
test suggest::tests::test_no_suggestions_for_wildly_different_strings ... ok
test tests::all_virtual_providers_conflict_reports_conflict_before_dependencies ... ok
test suggest::tests::test_suggest_obvious_typos ... ok
test tests::aur_alternative_is_built_before_its_dependent ... ok
test tests::aur_makedepends_ordered_before_dependent ... ok
test tests::alternatives_fallback_if_primary_fails ... ok
test tests::alternatives_fallback_under_conflict ... ok
test tests::aur_package_pulls_official_deps_and_makedeps ... ok
test tests::aur_cycle_is_reported_with_path ... ok
test tests::backtracks_virtual_provider_after_late_conflict ... ok
test tests::backtracking_restores_already_selected_dependencies ... ok
test tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies ... ok
test tests::aur_package_named_target_does_not_create_spurious_edges ... ok
test tests::debian_target_constraints_use_dpkg_backend ... ok
test tests::backtracks_existing_virtual_binding_to_avoid_false_aur_cycle ... ok
test tests::declared_conflicts_are_fatal ... ok
test tests::backtracking_recovers_from_missing_transitive_dependency ... ok
test tests::mixed_backend_resolution_returns_typed_error ... ok
test tests::linear_chain_resolves_fully ... ok
test tests::diamond_appears_once ... ok
test tests::not_found_names_the_dependent ... ok
test tests::official_cycles_are_tolerated ... ok
test tests::backtracking_recovers_from_aur_cycle ... ok
test tests::search_exhaustion_is_explicit ... ok
test tests::official_shadows_aur_same_name ... ok
test tests::official_provides_beats_aur_exact_name_match ... ok
test tests::prefer_installed_avoids_unnecessary_conflicting_swap ... ok
test tests::self_conflict_is_ignored ... ok
test tests::split_siblings_with_runtime_cycle_build_as_one_base ... ok
test tests::provides_resolved_package_is_also_found_by_concrete_name_in_same_batch ... ok
test tests::unsatisfiable_version_reports_whats_available ... ok
test tests::targets_have_empty_required_by ... ok
test tests::versioned_target_spec_is_honored ... ok
test tests::version_conflict_names_both_requirers ... ok
test tests::virtual_package_resolved_via_provides ... ok
test tests::virtual_provider_avoids_conflict_with_selected_package ... ok
test tests::virtual_provider_respects_reverse_conflict_even_when_installed ... ok
test tests::versioned_provide_checked_against_constraint ... ok

test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 28 tests
test dependency::tests::bare_provide_never_satisfies_versioned_dep ... ok
test dependency::tests::eq_constraint_without_pkgrel_matches_any_pkgrel ... ok
test dependency::tests::constraint_ops_match_vercmp ... ok
test dependency::tests::ge_is_not_gt_plus_eq ... ok
test dependency::tests::parses_ge_constraint ... ok
test dependency::tests::parses_unversioned ... ok
test dependency::tests::provide_name_must_match ... ok
test dependency::tests::unconstrained_dep_accepts_anything ... ok
test dependency::tests::strips_colon_description_suffix ... ok
test dependency::tests::versioned_provide_satisfies_versioned_dep ... ok
test package::tests::satisfies_by_exact_name_and_version ... ok
test package::tests::satisfies_via_provides ... ok
test version::tests::debian_tilde_sorts_older_than_anything ... ok
test version::tests::display_roundtrips ... ok
test version::tests::dpkg_version_comparisons ... ok
test version::tests::epoch_dominates_everything ... ok
test version::tests::leading_zeros_are_insignificant ... ok
test version::tests::man_page_alpha_chain ... ok
test version::tests::missing_pkgrel_matches_any_pkgrel ... ok
test version::tests::man_page_numeric_chain ... ok
test version::tests::mixed_backend_comparison_returns_without_panicking ... ok
test version::tests::numeric_compares_by_value_not_lexically ... ok
test version::tests::numeric_segment_beats_alphabetic ... ok
test version::tests::parses_bare_version ... ok
test version::tests::parses_epoch_ver_rel ... ok
test version::tests::pkgrel_breaks_ties ... ok
test version::tests::pkgrel_with_subrelease ... ok
test version::tests::separator_runs_compare_by_length ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/bin/verify_dpkg.rs (/tmp/archon-audit-resolver-target/debug/deps/verify_dpkg-6b538ebfda907eb1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_resolver

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_types

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### AUR check dependencies

```sh
python3 scripts/test-cached-crate.py archon-aur checkdepends_are_required_for_default_makepkg_checks
```

Before ([complete log](audit/evidence/completion-checkdepends-before.txt)):

```text

running 1 test

thread 'model::tests::checkdepends_are_required_for_default_makepkg_checks' (50) panicked at /home/ravi/archon/crates/archon-aur/src/model.rs:93:9:
assertion `left == right` failed
  left: ["cmake"]
 right: ["cmake", "pytest>=8"]
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test model::tests::checkdepends_are_required_for_default_makepkg_checks ... FAILED

failures:

failures:
    model::tests::checkdepends_are_required_for_default_makepkg_checks

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s
```

After ([complete log](audit/evidence/completion-checkdepends-after.txt)):

```text

running 3 tests
test model::tests::checkdepends_are_required_for_default_makepkg_checks ... ok
test model::tests::deserializes_rpc_json_and_converts ... ok
test model::tests::bad_version_error_names_the_package ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s
```

### Split metadata

```sh
python3 scripts/test-cached-crate.py archon-aur split_package_metadata_preserves_its_build_base
```

Before ([complete log](audit/evidence/completion-split-before.txt)):

```text

running 1 test

thread 'model::tests::split_package_metadata_preserves_its_build_base' (52) panicked at /home/ravi/archon/crates/archon-aur/src/model.rs:109:9:
assertion `left == right` failed
  left: None
 right: Some("suite")
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test model::tests::split_package_metadata_preserves_its_build_base ... FAILED

failures:

failures:
    model::tests::split_package_metadata_preserves_its_build_base

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.00s
```

After ([complete log](audit/evidence/completion-split-metadata-after.txt)):

```text

running 4 tests
test model::tests::deserializes_rpc_json_and_converts ... ok
test model::tests::split_package_metadata_preserves_its_build_base ... ok
test model::tests::bad_version_error_names_the_package ... ok
test model::tests::checkdepends_are_required_for_default_makepkg_checks ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s
```

### Split runtime cycles

```sh
scripts/test-resolver-offline.sh split_siblings_with_runtime_cycle_build_as_one_base
```

Before ([complete log](audit/evidence/completion-split-cycle-before.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.MkZRez/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.MkZRez/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.42s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 1 test

thread 'tests::split_siblings_with_runtime_cycle_build_as_one_base' (370) panicked at crates/archon-resolver/src/lib.rs:538:62:
called `Result::unwrap()` on an `Err` value: DependencyCycle { cycle: ["suite-a", "suite-b", "suite-a"] }
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test tests::split_siblings_with_runtime_cycle_build_as_one_base ... FAILED

failures:

failures:
    tests::split_siblings_with_runtime_cycle_build_as_one_base

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 37 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p archon-resolver --lib`
```

After ([complete log](audit/evidence/completion-split-resolver-after.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.JwQ56O/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.JwQ56O/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.50s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 38 tests
test suggest::tests::test_suggest_obvious_typos ... ok
test suggest::tests::test_no_suggestions_for_wildly_different_strings ... ok
test tests::all_virtual_providers_conflict_reports_conflict_before_dependencies ... ok
test tests::alternatives_fallback_if_primary_fails ... ok
test tests::aur_alternative_is_built_before_its_dependent ... ok
test tests::alternatives_fallback_under_conflict ... ok
test tests::aur_package_named_target_does_not_create_spurious_edges ... ok
test tests::aur_build_order_is_dependency_first ... ok
test tests::aur_makedepends_ordered_before_dependent ... ok
test tests::aur_cycle_is_reported_with_path ... ok
test tests::aur_package_pulls_official_deps_and_makedeps ... ok
test tests::backtracking_recovers_from_missing_transitive_dependency ... ok
test tests::mixed_backend_resolution_returns_typed_error ... ok
test tests::declared_conflicts_are_fatal ... ok
test tests::backtracking_recovers_from_aur_cycle ... ok
test tests::linear_chain_resolves_fully ... ok
test tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies ... ok
test tests::debian_target_constraints_use_dpkg_backend ... ok
test tests::backtracking_restores_already_selected_dependencies ... ok
test tests::official_provides_beats_aur_exact_name_match ... ok
test tests::backtracks_virtual_provider_after_late_conflict ... ok
test tests::search_exhaustion_is_explicit ... ok
test tests::provides_resolved_package_is_also_found_by_concrete_name_in_same_batch ... ok
test tests::official_cycles_are_tolerated ... ok
test tests::official_shadows_aur_same_name ... ok
test tests::not_found_names_the_dependent ... ok
test tests::targets_have_empty_required_by ... ok
test tests::unsatisfiable_version_reports_whats_available ... ok
test tests::split_siblings_with_runtime_cycle_build_as_one_base ... ok
test tests::diamond_appears_once ... ok
test tests::version_conflict_names_both_requirers ... ok
test tests::prefer_installed_avoids_unnecessary_conflicting_swap ... ok
test tests::versioned_target_spec_is_honored ... ok
test tests::self_conflict_is_ignored ... ok
test tests::versioned_provide_checked_against_constraint ... ok
test tests::virtual_provider_avoids_conflict_with_selected_package ... ok
test tests::virtual_provider_respects_reverse_conflict_even_when_installed ... ok
test tests::virtual_package_resolved_via_provides ... ok

test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 28 tests
test dependency::tests::parses_ge_constraint ... ok
test dependency::tests::bare_provide_never_satisfies_versioned_dep ... ok
test package::tests::satisfies_via_provides ... ok
test dependency::tests::eq_constraint_without_pkgrel_matches_any_pkgrel ... ok
test dependency::tests::unconstrained_dep_accepts_anything ... ok
test dependency::tests::versioned_provide_satisfies_versioned_dep ... ok
test dependency::tests::constraint_ops_match_vercmp ... ok
test version::tests::display_roundtrips ... ok
test version::tests::epoch_dominates_everything ... ok
test version::tests::dpkg_version_comparisons ... ok
test dependency::tests::strips_colon_description_suffix ... ok
test version::tests::leading_zeros_are_insignificant ... ok
test version::tests::man_page_numeric_chain ... ok
test version::tests::man_page_alpha_chain ... ok
test dependency::tests::ge_is_not_gt_plus_eq ... ok
test version::tests::debian_tilde_sorts_older_than_anything ... ok
test dependency::tests::provide_name_must_match ... ok
test package::tests::satisfies_by_exact_name_and_version ... ok
test dependency::tests::parses_unversioned ... ok
test version::tests::missing_pkgrel_matches_any_pkgrel ... ok
test version::tests::mixed_backend_comparison_returns_without_panicking ... ok
test version::tests::numeric_compares_by_value_not_lexically ... ok
test version::tests::numeric_segment_beats_alphabetic ... ok
test version::tests::parses_bare_version ... ok
test version::tests::parses_epoch_ver_rel ... ok
test version::tests::pkgrel_breaks_ties ... ok
test version::tests::pkgrel_with_subrelease ... ok
test version::tests::separator_runs_compare_by_length ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/bin/verify_dpkg.rs (/tmp/archon-audit-resolver-target/debug/deps/verify_dpkg-6b538ebfda907eb1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_resolver

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_types

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### Split installation

```sh
python3 scripts/repro-aur-install.py --direct split-base
```

Before ([complete log](audit/evidence/completion-split-install-before.txt)):

```text
$ direct-production-path install fixture-app --yes
case: split-base

↺ fixture-lib already cloned
✓ fixture-lib (Fetch)
✓ fixture-lib (Build)
✓ fixture-lib built
==> Installing fixture-lib via pacman -U
↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
==> Installing fixture-app via pacman -U

Installed 2 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-l4k6i8k5/cache/archon/build/fixture-lib/fixture-lib-1-1-x86_64.pkg.tar.zst'], ['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-l4k6i8k5/cache/archon/build/fixture-app/fixture-app-1-1-x86_64.pkg.tar.zst']]
Traceback (most recent call last):
  File "/home/ravi/archon/scripts/repro-aur-install.py", line 225, in <module>
    for scenario in scenarios: run_case(scenario)
                               ~~~~~~~~^^^^^^^^^^
  File "/home/ravi/archon/scripts/repro-aur-install.py", line 135, in run_case
    assert len(calls) == 1 and len(calls[0]) == 6, 'split outputs must install together'
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: split outputs must install together
```

After ([complete log](audit/evidence/completion-split-install-after.txt)):

```text
$ direct-production-path install fixture-app --yes
case: split-base

↺ fixture-lib already cloned
✓ suite (Fetch)
✓ suite (Build)
✓ suite built
==> Installing fixture-lib via pacman -U

Installed 2 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-mnri7a_e/cache/archon/build/suite/fixture-lib-1-1-x86_64.pkg.tar.zst', '/tmp/archon-aur-install-mnri7a_e/cache/archon/build/suite/fixture-app-1-1-x86_64.pkg.tar.zst']]
PASS: split-base
```

### Archive identity

```sh
python3 scripts/repro-aur-install.py --direct archive-identity
```

Before ([complete log](audit/evidence/completion-artifact-before.txt)):

```text
$ direct-production-path install fixture-app --yes
case: archive-identity

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
==> Installing fixture-app via pacman -U

Installed 1 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-v9lsoiub/cache/archon/build/fixture-app/fixture-app-addon-1-1-x86_64.pkg.tar.zst']]
Traceback (most recent call last):
  File "/home/ravi/archon/scripts/repro-aur-install.py", line 235, in <module>
    for scenario in scenarios: run_case(scenario)
                               ~~~~~~~~^^^^^^^^^^
  File "/home/ravi/archon/scripts/repro-aur-install.py", line 157, in run_case
    assert result.returncode != 0 and not calls, 'failed build/missing output must not install'
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: failed build/missing output must not install
```

After ([complete log](audit/evidence/completion-artifact-after.txt)):

```text
$ direct-production-path install fixture-app --yes
case: success

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
==> Installing fixture-app via pacman -U

Installed 1 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-xfvh1216/cache/archon/build/fixture-app/fixture-app-1-1-x86_64.pkg.tar.zst']]
PASS: success
$ direct-production-path install fixture-app --yes
case: install-failure

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
==> Installing fixture-app via pacman -U
Error: pacman -U exited with exit status: 23 for fixture-app
exit: 1
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-uoh6_6_g/cache/archon/build/fixture-app/fixture-app-1-1-x86_64.pkg.tar.zst']]
PASS: install-failure
$ direct-production-path install fixture-app --yes
case: build-failure

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✗ fixture-app failed (Build)
Error: sandboxed build failed for fixture-app (Build stage)
exit: 1
mock privileged argv: []
PASS: build-failure
$ direct-production-path install fixture-app --yes
case: missing-artifact

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
Error: expected exactly one built package archive for fixture-app in /tmp/archon-aur-install-5o9fye8x/cache/archon/build/fixture-app, found 0; refusing installation of missing or ambiguous output
exit: 1
mock privileged argv: []
PASS: missing-artifact
$ direct-production-path install fixture-app --yes
case: signed-artifact

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
==> Installing fixture-app via pacman -U

Installed 1 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-0uxd7vac/cache/archon/build/fixture-app/fixture-app-1-1-x86_64.pkg.tar.zst']]
PASS: signed-artifact
$ direct-production-path install fixture-app --yes
case: ambiguous-artifact

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
Error: expected exactly one built package archive for fixture-app in /tmp/archon-aur-install-uhylo5x2/cache/archon/build/fixture-app, found 2; refusing installation of missing or ambiguous output
exit: 1
mock privileged argv: []
PASS: ambiguous-artifact
$ direct-production-path install fixture-app --yes
case: dependency-chain

↺ z-lib already cloned
✓ z-lib (Fetch)
✓ z-lib (Build)
✓ z-lib built
==> Installing z-lib via pacman -U
↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
==> Installing fixture-app via pacman -U

Installed 2 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-fwsz1me0/cache/archon/build/z-lib/z-lib-1-1-x86_64.pkg.tar.zst'], ['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-fwsz1me0/cache/archon/build/fixture-app/fixture-app-1-1-x86_64.pkg.tar.zst']]
PASS: dependency-chain
$ direct-production-path install fixture-app --yes
case: split-base

↺ fixture-lib already cloned
✓ suite (Fetch)
✓ suite (Build)
✓ suite built
==> Installing fixture-lib via pacman -U

Installed 2 AUR package(s).
exit: 0
mock privileged argv: [['pacman', '-U', '--needed', '--', '/tmp/archon-aur-install-qdnglkfu/cache/archon/build/suite/fixture-lib-1-1-x86_64.pkg.tar.zst', '/tmp/archon-aur-install-qdnglkfu/cache/archon/build/suite/fixture-app-1-1-x86_64.pkg.tar.zst']]
PASS: split-base
$ direct-production-path install fixture-app --yes
case: archive-identity

↺ fixture-app already cloned
✓ fixture-app (Fetch)
✓ fixture-app (Build)
✓ fixture-app built
Error: expected exactly one built package archive for fixture-app in /tmp/archon-aur-install-89pvypvb/cache/archon/build/fixture-app, found 0; refusing installation of missing or ambiguous output
exit: 1
mock privileged argv: []
PASS: archive-identity
```

### Unicode/backend-isolated correction

```sh
python3 scripts/repro-typos.py
```

Before ([complete log](audit/evidence/completion-typos-before.txt)):

```text
$ direct-production-typo-path 'éé'

thread 'main' (44) panicked at /tmp/archon-typos-ke6d5h67/install.rs:323:44:
end byte index 3 is not a char boundary; it is inside 'é' (bytes 2..4 of string)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
exit: 101
FAIL: unicode
$ direct-production-typo-path 'debianonl'
package not found, did you mean: debianonly? re-run interactively or with the exact name
exit: 1
FAIL: backend-isolation
Regression failures: unicode, backend-isolation
```

After ([complete log](audit/evidence/completion-typos-after.txt)):

```text
$ direct-production-typo-path 'éé'
exit: 0
PASS: unicode
$ direct-production-typo-path 'debianonl'
exit: 0
PASS: backend-isolation
```

### Suggestion determinism

```sh
scripts/test-resolver-offline.sh suggestion_ties_do_not_depend_on_index_iteration_order
```

Before ([complete log](audit/evidence/completion-suggestion-order-before.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.BUxS0D/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.BUxS0D/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.41s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 1 test

thread 'suggest::tests::suggestion_ties_do_not_depend_on_index_iteration_order' (370) panicked at crates/archon-resolver/src/suggest.rs:68:9:
assertion `left == right` failed
  left: [Suggestion { pkg: PackageRef { name: "foo", source: "pacman", repo: Some("extra") }, similarity: 0.9111111111111111 }, Suggestion { pkg: PackageRef { name: "foa", source: "pacman", repo: Some("extra") }, similarity: 0.9111111111111111 }]
 right: [Suggestion { pkg: PackageRef { name: "foa", source: "pacman", repo: Some("extra") }, similarity: 0.9111111111111111 }, Suggestion { pkg: PackageRef { name: "foo", source: "pacman", repo: Some("extra") }, similarity: 0.9111111111111111 }]
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test suggest::tests::suggestion_ties_do_not_depend_on_index_iteration_order ... FAILED

failures:

failures:
    suggest::tests::suggestion_ties_do_not_depend_on_index_iteration_order

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 38 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p archon-resolver --lib`
```

After ([complete log](audit/evidence/completion-suggestion-order-after.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.VwhIix/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.VwhIix/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.68s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 39 tests
test suggest::tests::test_no_suggestions_for_wildly_different_strings ... ok
test suggest::tests::suggestion_ties_do_not_depend_on_index_iteration_order ... ok
test tests::all_virtual_providers_conflict_reports_conflict_before_dependencies ... ok
test suggest::tests::test_suggest_obvious_typos ... ok
test tests::alternatives_fallback_if_primary_fails ... ok
test tests::aur_package_pulls_official_deps_and_makedeps ... ok
test tests::aur_alternative_is_built_before_its_dependent ... ok
test tests::aur_makedepends_ordered_before_dependent ... ok
test tests::aur_package_named_target_does_not_create_spurious_edges ... ok
test tests::backtracking_recovers_from_missing_transitive_dependency ... ok
test tests::aur_cycle_is_reported_with_path ... ok
test tests::aur_build_order_is_dependency_first ... ok
test tests::debian_target_constraints_use_dpkg_backend ... ok
test tests::alternatives_fallback_under_conflict ... ok
test tests::backtracking_recovers_from_aur_cycle ... ok
test tests::backtracks_virtual_provider_after_late_conflict ... ok
test tests::backtracking_restores_already_selected_dependencies ... ok
test tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies ... ok
test tests::declared_conflicts_are_fatal ... ok
test tests::mixed_backend_resolution_returns_typed_error ... ok
test tests::linear_chain_resolves_fully ... ok
test tests::official_cycles_are_tolerated ... ok
test tests::not_found_names_the_dependent ... ok
test tests::official_shadows_aur_same_name ... ok
test tests::official_provides_beats_aur_exact_name_match ... ok
test tests::diamond_appears_once ... ok
test tests::prefer_installed_avoids_unnecessary_conflicting_swap ... ok
test tests::provides_resolved_package_is_also_found_by_concrete_name_in_same_batch ... ok
test tests::split_siblings_with_runtime_cycle_build_as_one_base ... ok
test tests::versioned_target_spec_is_honored ... ok
test tests::search_exhaustion_is_explicit ... ok
test tests::self_conflict_is_ignored ... ok
test tests::targets_have_empty_required_by ... ok
test tests::version_conflict_names_both_requirers ... ok
test tests::versioned_provide_checked_against_constraint ... ok
test tests::unsatisfiable_version_reports_whats_available ... ok
test tests::virtual_package_resolved_via_provides ... ok
test tests::virtual_provider_avoids_conflict_with_selected_package ... ok
test tests::virtual_provider_respects_reverse_conflict_even_when_installed ... ok

test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 28 tests
test dependency::tests::bare_provide_never_satisfies_versioned_dep ... ok
test dependency::tests::parses_unversioned ... ok
test dependency::tests::constraint_ops_match_vercmp ... ok
test dependency::tests::eq_constraint_without_pkgrel_matches_any_pkgrel ... ok
test dependency::tests::ge_is_not_gt_plus_eq ... ok
test dependency::tests::parses_ge_constraint ... ok
test dependency::tests::provide_name_must_match ... ok
test dependency::tests::strips_colon_description_suffix ... ok
test dependency::tests::unconstrained_dep_accepts_anything ... ok
test dependency::tests::versioned_provide_satisfies_versioned_dep ... ok
test package::tests::satisfies_by_exact_name_and_version ... ok
test package::tests::satisfies_via_provides ... ok
test version::tests::debian_tilde_sorts_older_than_anything ... ok
test version::tests::display_roundtrips ... ok
test version::tests::epoch_dominates_everything ... ok
test version::tests::dpkg_version_comparisons ... ok
test version::tests::leading_zeros_are_insignificant ... ok
test version::tests::missing_pkgrel_matches_any_pkgrel ... ok
test version::tests::man_page_alpha_chain ... ok
test version::tests::numeric_segment_beats_alphabetic ... ok
test version::tests::parses_bare_version ... ok
test version::tests::numeric_compares_by_value_not_lexically ... ok
test version::tests::parses_epoch_ver_rel ... ok
test version::tests::man_page_numeric_chain ... ok
test version::tests::pkgrel_with_subrelease ... ok
test version::tests::pkgrel_breaks_ties ... ok
test version::tests::separator_runs_compare_by_length ... ok
test version::tests::mixed_backend_comparison_returns_without_panicking ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/bin/verify_dpkg.rs (/tmp/archon-audit-resolver-target/debug/deps/verify_dpkg-6b538ebfda907eb1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_resolver

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_types

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### Inconclusive monitor report

```sh
python3 scripts/test-cached-crate.py archon-monitor empty_monitor_report_does_not_authorize_installation
```

Before ([complete log](audit/evidence/completion-empty-monitor-before.txt)):

```text

running 1 test

thread 'report::tests::empty_monitor_report_does_not_authorize_installation' (45) panicked at /home/ravi/archon/crates/archon-monitor/src/report.rs:57:9:
capturing no events is inconclusive, not a clean build
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test report::tests::empty_monitor_report_does_not_authorize_installation ... FAILED

failures:

failures:
    report::tests::empty_monitor_report_does_not_authorize_installation

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 16 filtered out; finished in 0.00s
```

After ([complete log](audit/evidence/completion-empty-monitor-after.txt)):

```text

running 17 tests
test policy::tests::allow_listed_tool_passes ... ok
test policy::tests::custom_allowed_tool ... ok
test policy::tests::declared_connection_passes ... ok
test policy::tests::undeclared_connection_is_flagged ... ok
test policy::tests::readonly_open_outside_build_dir_passes ... ok
test policy::tests::dev_null_write_is_exempt ... ok
test policy::tests::exec_inside_build_dir_passes ... ok
test policy::tests::namespace_setup_files_are_exempt ... ok
test report::tests::clean_report_says_so ... ok
test policy::tests::unexpected_exec_is_flagged ... ok
test policy::tests::write_open_inside_build_dir_passes ... ok
test policy::tests::write_open_outside_build_dir_is_flagged ... ok
test report::tests::empty_monitor_report_does_not_authorize_installation ... ok
test policy::tests::bwrap_and_makepkg_internals_are_allow_listed ... ok
test report::tests::flagged_report_lists_reasons ... ok
test store::tests::records_and_reads_back_events ... ok
test store::tests::separate_builds_dont_mix_events ... ok

test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

### Native ALPM tilde comparison

```sh
scripts/test-resolver-offline.sh alpm_tilde_is_a_separator_not_debian_prerelease
```

Before ([complete log](audit/evidence/completion-alpm-test-before.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.KigftN/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.KigftN/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.62s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 40 filtered out; finished in 0.00s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 1 test

thread 'version::tests::alpm_tilde_is_a_separator_not_debian_prerelease' (1305) panicked at crates/archon-types/src/version.rs:448:9:
assertion `left == right` failed
  left: Less
 right: Greater
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test version::tests::alpm_tilde_is_a_separator_not_debian_prerelease ... FAILED

failures:

failures:
    version::tests::alpm_tilde_is_a_separator_not_debian_prerelease

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 28 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p archon-types --lib`
```

After ([complete log](audit/evidence/completion-alpm-test-after.txt)):

```text
     Locking 10 packages to highest compatible versions
   Compiling archon-types v0.2.0 (/tmp/archon-resolver-check.vUUXkK/crates/archon-types)
   Compiling archon-resolver v0.2.0 (/tmp/archon-resolver-check.vUUXkK/crates/archon-resolver)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.35s
     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_resolver-f93912bda7b36f67)

running 40 tests
test suggest::tests::test_no_suggestions_for_wildly_different_strings ... ok
test suggest::tests::suggestion_ties_do_not_depend_on_index_iteration_order ... ok
test suggest::tests::test_suggest_obvious_typos ... ok
test tests::all_virtual_providers_conflict_reports_conflict_before_dependencies ... ok
test tests::aur_alternative_is_built_before_its_dependent ... ok
test tests::aur_cycle_is_reported_with_path ... ok
test tests::alternatives_fallback_if_primary_fails ... ok
test tests::aur_package_named_target_does_not_create_spurious_edges ... ok
test tests::alternatives_fallback_under_conflict ... ok
test tests::aur_build_order_is_dependency_first ... ok
test tests::declared_conflicts_are_fatal ... ok
test tests::backtracks_alternative_after_late_conflict_without_leaked_dependencies ... ok
test tests::debian_target_constraints_use_dpkg_backend ... ok
test tests::backtracking_recovers_from_missing_transitive_dependency ... ok
test tests::aur_makedepends_ordered_before_dependent ... ok
test tests::aur_package_pulls_official_deps_and_makedeps ... ok
test tests::backtracking_recovers_from_aur_cycle ... ok
test tests::backtracking_restores_already_selected_dependencies ... ok
test tests::backtracks_virtual_provider_after_late_conflict ... ok
test tests::diamond_appears_once ... ok
test tests::backtracks_existing_virtual_binding_to_avoid_false_aur_cycle ... ok
test tests::mixed_backend_resolution_returns_typed_error ... ok
test tests::not_found_names_the_dependent ... ok
test tests::linear_chain_resolves_fully ... ok
test tests::official_shadows_aur_same_name ... ok
test tests::official_cycles_are_tolerated ... ok
test tests::official_provides_beats_aur_exact_name_match ... ok
test tests::search_exhaustion_is_explicit ... ok
test tests::targets_have_empty_required_by ... ok
test tests::prefer_installed_avoids_unnecessary_conflicting_swap ... ok
test tests::provides_resolved_package_is_also_found_by_concrete_name_in_same_batch ... ok
test tests::version_conflict_names_both_requirers ... ok
test tests::versioned_target_spec_is_honored ... ok
test tests::split_siblings_with_runtime_cycle_build_as_one_base ... ok
test tests::virtual_package_resolved_via_provides ... ok
test tests::unsatisfiable_version_reports_whats_available ... ok
test tests::versioned_provide_checked_against_constraint ... ok
test tests::virtual_provider_avoids_conflict_with_selected_package ... ok
test tests::virtual_provider_respects_reverse_conflict_even_when_installed ... ok
test tests::self_conflict_is_ignored ... ok

test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/lib.rs (/tmp/archon-audit-resolver-target/debug/deps/archon_types-810f58dc673882ba)

running 29 tests
test dependency::tests::bare_provide_never_satisfies_versioned_dep ... ok
test dependency::tests::constraint_ops_match_vercmp ... ok
test dependency::tests::ge_is_not_gt_plus_eq ... ok
test dependency::tests::eq_constraint_without_pkgrel_matches_any_pkgrel ... ok
test dependency::tests::parses_unversioned ... ok
test dependency::tests::provide_name_must_match ... ok
test dependency::tests::strips_colon_description_suffix ... ok
test dependency::tests::parses_ge_constraint ... ok
test dependency::tests::unconstrained_dep_accepts_anything ... ok
test dependency::tests::versioned_provide_satisfies_versioned_dep ... ok
test package::tests::satisfies_by_exact_name_and_version ... ok
test package::tests::satisfies_via_provides ... ok
test version::tests::alpm_tilde_is_a_separator_not_debian_prerelease ... ok
test version::tests::debian_tilde_sorts_older_than_anything ... ok
test version::tests::display_roundtrips ... ok
test version::tests::dpkg_version_comparisons ... ok
test version::tests::epoch_dominates_everything ... ok
test version::tests::leading_zeros_are_insignificant ... ok
test version::tests::man_page_numeric_chain ... ok
test version::tests::man_page_alpha_chain ... ok
test version::tests::missing_pkgrel_matches_any_pkgrel ... ok
test version::tests::mixed_backend_comparison_returns_without_panicking ... ok
test version::tests::numeric_compares_by_value_not_lexically ... ok
test version::tests::numeric_segment_beats_alphabetic ... ok
test version::tests::parses_bare_version ... ok
test version::tests::parses_epoch_ver_rel ... ok
test version::tests::pkgrel_breaks_ties ... ok
test version::tests::pkgrel_with_subrelease ... ok
test version::tests::separator_runs_compare_by_length ... ok

test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/bin/verify_dpkg.rs (/tmp/archon-audit-resolver-target/debug/deps/verify_dpkg-6b538ebfda907eb1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_resolver

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests archon_types

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### Comparator checker exit status

```sh
python3 scripts/repro-dpkg-checker.py
```

Before ([complete log](audit/evidence/completion-dpkg-exit-before.txt)):

```text
$ verify_dpkg (deliberately incorrect mocked dpkg)
MISMATCH: 1.0~rc1 vs 1.0 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc1 vs 1.0.1 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc1 vs 1.0~rc2 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc2 vs 1.0~rc1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0~rc1~2 vs 1.0~rc1 -> Rust says Less, dpkg says Equal
MATCH: 1~rc1 vs 1~rc1 -> Equal
MISMATCH: 1.0~~ vs 1.0~ -> Rust says Less, dpkg says Equal
MISMATCH: 1:2.0 vs 2.0~rc1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0 vs 1.0~rc1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1 vs 1.0-2 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0a vs 1.0 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1ubuntu1 vs 1.0-1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1ubuntu1 vs 1.0-1ubuntu2 -> Rust says Less, dpkg says Equal
MISMATCH: 2.0-1 vs 1:1.0-1 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0-1 vs 1.0-1+b1 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0+nmu1 vs 1.0 -> Rust says Greater, dpkg says Equal
MISMATCH: 0.99 vs 1.0~alpha -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~alpha vs 1.0~beta -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~beta vs 1.0~rc -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc vs 1.0 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0.0 vs 1.0 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0 vs 1.0.0 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0-1 vs 1.0-0.1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1~bpo10+1 vs 1.0-1 -> Rust says Less, dpkg says Equal
MATCH: 1.2.3-4 vs 1.2.3-4 -> Equal
23 mismatches found.
exit: 0
Traceback (most recent call last):
  File "/home/ravi/archon/scripts/repro-dpkg-checker.py", line 19, in <module>
    assert result.returncode != 0, 'mismatched verification must fail, not exit successfully'
           ^^^^^^^^^^^^^^^^^^^^^^
AssertionError: mismatched verification must fail, not exit successfully
```

After ([complete log](audit/evidence/completion-dpkg-exit-after.txt)):

```text
$ verify_dpkg (deliberately incorrect mocked dpkg)
MISMATCH: 1.0~rc1 vs 1.0 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc1 vs 1.0.1 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc1 vs 1.0~rc2 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc2 vs 1.0~rc1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0~rc1~2 vs 1.0~rc1 -> Rust says Less, dpkg says Equal
MATCH: 1~rc1 vs 1~rc1 -> Equal
MISMATCH: 1.0~~ vs 1.0~ -> Rust says Less, dpkg says Equal
MISMATCH: 1:2.0 vs 2.0~rc1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0 vs 1.0~rc1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1 vs 1.0-2 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0a vs 1.0 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1ubuntu1 vs 1.0-1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1ubuntu1 vs 1.0-1ubuntu2 -> Rust says Less, dpkg says Equal
MISMATCH: 2.0-1 vs 1:1.0-1 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0-1 vs 1.0-1+b1 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0+nmu1 vs 1.0 -> Rust says Greater, dpkg says Equal
MISMATCH: 0.99 vs 1.0~alpha -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~alpha vs 1.0~beta -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~beta vs 1.0~rc -> Rust says Less, dpkg says Equal
MISMATCH: 1.0~rc vs 1.0 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0.0 vs 1.0 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0 vs 1.0.0 -> Rust says Less, dpkg says Equal
MISMATCH: 1.0-1 vs 1.0-0.1 -> Rust says Greater, dpkg says Equal
MISMATCH: 1.0-1~bpo10+1 vs 1.0-1 -> Rust says Less, dpkg says Equal
MATCH: 1.2.3-4 vs 1.2.3-4 -> Equal
23 mismatches found.
exit: 1
PASS: mismatches produce a failing exit status
```

## Historical Phase 0 and earlier implementation notes

**Historical statuses below are superseded by the current disposition above.**

# Archon audit — initial Phase 0, incomplete

Date: 2026-10-19. Baseline: c09eb15. No host packages installed or removed.
Actual environment: unprivileged uid 1000, Arch Linux, makepkg/pacman present;
Docker and Podman absent. This contradicts the supplied Ubuntu description.
The source predates this audit; authorship and prior live-verification claims
in comments/README have not been independently established.

## Findings

| Area / severity | Location | Finding / reproducer | Status |
|---|---|---|---|
| A / high | crates/archon-distro/src/lib.rs:68 | ID and ID_LIKE were merged and searched in hardcoded family order. `ID=ubuntu\nID_LIKE=arch` selected Arch. `ID=custom\nID_LIKE="debian arch"` also selected Arch. | fixed; two regression tests failed first, then passed |
| A / medium | crates/archon-distro/src/lib.rs:53 | Unreadable-path diagnostic always names /etc/os-release even with ARCHON_OS_RELEASE override; original user's unreadable-file symptom not reproduced | unfixed-with-reason: needs failing CLI reproduction once build is available |
| A / medium | crates/archon/src/commands/build.rs:25; crates/archon-sandbox/src/runner.rs:174 | Local build always invokes makepkg; no distro/makepkg preflight. | needs-decision: reject non-Arch or managed Arch container; runtime symptom UNVERIFIED |
| A / medium | crates/archon/src/commands/install.rs:110 | Non-Arch installs delegate; AptLists is not used for install resolution. `--report` warns then proceeds with ordinary installation. | needs-decision: native delegation vs Archon resolution; report policy still to audit |
| E / high | crates/archon/src/commands/install.rs:320 | `&typo[..3]` slices a byte boundary; multi-byte Unicode can panic. | unfixed-with-reason: runtime regression not yet run; Cargo blocked |
| E / medium | crates/archon/src/commands/install.rs:325 | Arch typo candidates include AptLists, violating backend isolation. | unfixed-with-reason: CLI regression not yet run; Cargo blocked |
| D / high | crates/archon-types/src/version.rs:91 | Cross-backend vercmp explicitly panics. | UNVERIFIED: user-reachable graph path not yet established |
| B / medium | crates/archon-repo/src/lib.rs:28; crates/archon/src/commands/install.rs:185 | Install passes fixed core/extra/multilib order; custom pacman.conf integration needs review. | UNVERIFIED: no custom-repo reproduction yet |
| F / high | crates/archon-monitor-ebpf/src/main.rs:35,100,154 | Fixed tracepoint offsets require running-kernel validation. | UNVERIFIED: no privileged checks authorized/run |
| G / low | workspace | cargo fmt --check emits diffs across existing source. | unfixed-with-reason: broad formatting would obscure per-bug changes |

These are initial findings, not a completed audit. A/B/D/E need further
regressions. C (RPC, dependency ordering), D (alternatives, conflicts,
determinism, timings), F (sandbox/monitor lifecycle), G (all panic sites,
CLI side effects), and H (injection, traversal, TOCTOU, untrusted metadata)
remain UNVERIFIED beyond the specific source observations above. No claim
of safe arbitrary-package installation is made.

## Raw evidence and exact commands

Detection regression, before implementation changes:

```sh
rustc --edition=2024 --test crates/archon-distro/src/lib.rs -o /tmp/archon-distro-audit-tests
/tmp/archon-distro-audit-tests --test-threads=1
```

Raw output: [before](audit/evidence/detection-before.txt),
[after](audit/evidence/detection-after.txt). Before: 25 passed, 2 failed.
After: 27 passed, 0 failed. This crate has no dependencies, so compiling its
actual lib.rs with rustc runs its actual unit tests without Cargo resolution.
This does not verify the CLI or other crates.

```sh
cargo test --workspace
```

Failed before compilation: crates.io DNS resolution failed downloading
`tinyvec_macros`. Subsequent offline commands and raw outputs:

```sh
cargo test --workspace --offline
cargo clippy --workspace --offline -- -D warnings
cargo fmt --check
scripts/matrix.sh
```

See [workspace test](audit/evidence/workspace-test.txt),
[clippy](audit/evidence/clippy.txt), [fmt](audit/evidence/fmt.txt),
[matrix table](audit/evidence/matrix-table.txt). Tests/clippy are UNVERIFIED
because a required dependency is missing. Formatting check fails.
No tests were deleted, weakened, or skipped.

## Matrix scope and privilege requirements

scripts/matrix.sh builds a static musl Archon in Docker, then exercises the
same CLI smoke scenarios on all six requested real images. Arch image
bootstrap includes base-devel, git, bubblewrap. Package bootstrap happens
only inside image builds. Shell syntax was checked; Dockerfiles and runtime
behavior are UNVERIFIED because Docker is absent. The script returns 2 when
Docker is missing and 1 on build/scenario failure; full logs are retained.

This is a smoke matrix, NOT the complete requested package-install matrix.
It covers CLI availability, real non-Arch detection and dry-run dispatch,
a precedence fixture, and unsupported-distro failure. It does not prove
actual downloading, Arch DB resolution, bwrap builds, or eBPF detection.

| Image | Result |
|---|---|
| archlinux:latest | UNVERIFIED: Docker absent |
| ubuntu:24.04 | UNVERIFIED: Docker absent |
| debian:stable | UNVERIFIED: Docker absent |
| fedora:latest | UNVERIFIED: Docker absent |
| opensuse/tumbleweed | UNVERIFIED: Docker absent |
| alpine:latest | UNVERIFIED: Docker absent |

Smoke scenarios need no --privileged or cgroup access. Image builds need
network and container package installation. Real bwrap builds need permitted
user/mount namespaces; Docker seccomp/AppArmor may require specific changes.
eBPF tests need root/capabilities permitted by the kernel, tracefs access,
a writable delegated cgroup v2 hierarchy, and a built BPF object. Broad
--privileged is a possible dedicated test environment, not enabled by this
script; ask before running it. No evil PKGBUILD was executed in this audit.

## Decisions pending

1. Keep apt's native resolver or complete and enable Archon's Debian resolver?
2. Reject non-Arch local PKGBUILD builds or design managed Arch containers?

Further dependent implementation is paused for these explicit design choices,
as requested. The overall audit is unfinished; environment restoration is
also required for full validation.

## Commit blocker

Both requested commit attempts were rejected by Git: `Author identity unknown`
and `fatal: unable to auto-detect email address (got 'ravi@sai.(none)')`.
No author identity was invented or configured. Changes remain uncommitted;
user name/email are required to create the separate commits.

## Follow-up: resolver and security-package investigation

The user reports recurring security-package failures but has not yet supplied
package names, exact commands, distro, or error output. No specific security
package failure has therefore been reproduced. The following confirmed bugs
use hermetic synthetic metadata and the actual production source files.

| Severity | Location | Root cause | Status |
|---|---|---|---|
| high | crates/archon-resolver/src/lib.rs:392 | AUR graph construction re-resolved original dependency names, losing selected alternatives and potentially choosing a different virtual provider. | fixed in source; failed-first regression; build plan now uses recorded concrete choices |
| high | crates/archon-resolver/src/lib.rs:148 | Virtual candidates were ranked by installed/alphabetic preference without checking conflicts with already selected packages. | fixed in source for previously selected packages; tests cover both conflict directions and an installed conflicting provider |
| medium | crates/archon-resolver/src/lib.rs:226 | Conflicting candidates without alternatives were allowed to enqueue dependencies, hiding the conflict behind unrelated NotFound errors. | fixed in source; reject before traversing dependencies |
| medium | crates/archon/src/srcinfo.rs:62 | Source-host extraction ignored makepkg VCS prefixes such as git+https and hg+https, leaving declared source hosts out of the monitor allow set. | fixed in source; parser regression failed first, then passed; live monitor behavior UNVERIFIED |
| low | crates/archon-types/src/version.rs:284,293 | Identical comparator branches trigger clippy if_same_then_else with -D warnings. | unfixed-with-reason: existing comparator source unchanged in this follow-up; isolated clippy fails |

### Exact reproduction commands and raw output

```sh
scripts/test-resolver-offline.sh aur_alternative_is_built_before_its_dependent -- --nocapture
```

[Raw before](audit/evidence/resolver-alternative-before.txt):
`left: ["a-scanner", "z-library"]`, `right: ["z-library", "a-scanner"]`;
1 failed. [Raw after](audit/evidence/resolver-alternative-after.txt):
regression passes, 25 resolver and 27 types tests pass.

```sh
scripts/test-resolver-offline.sh virtual_provider_ -- --nocapture
```

[Raw before](audit/evidence/resolver-provider-before.txt):
`NotFound { name: "rejected-only-dependency", required_by: "a-tls" }`
and `PackageConflict { a: "openssl", b: "a-tls", declared_by: "openssl" }`;
2 failed. A compatible z-tls candidate exists in both fixtures.
[Raw after](audit/evidence/resolver-provider-after.txt):
27 resolver and 27 types tests pass, including both new regressions.

```sh
scripts/test-resolver-offline.sh all_virtual_providers_conflict -- --nocapture
```

[Raw before](audit/evidence/resolver-conflict-before.txt):
`Err(NotFound { name: "unavailable", required_by: "a-tls" })`; 1 failed.
[Raw final after](audit/evidence/resolver-final.txt):
29 resolver and 27 types tests pass, including a PackageConflict assertion.
Also checks that an AUR package literally named target does not create
spurious graph edges from the internal TARGET label.

```sh
rustc --edition=2024 --test crates/archon/src/srcinfo.rs -o /tmp/archon-srcinfo-audit-tests
/tmp/archon-srcinfo-audit-tests --test-threads=1
```

[Raw before](audit/evidence/srcinfo-vcs-before.txt):
`missing declared Git source host: {}`; 4 passed, 1 failed.
[Raw after](audit/evidence/srcinfo-vcs-after.txt): 5 passed, 0 failed.
No DNS requests or untrusted builds are needed for this parser regression.

```sh
scripts/test-resolver-offline.sh
scripts/test-resolver-offline.sh --clippy -- -D warnings
cargo test --workspace --offline
cargo clippy --workspace --offline -- -D warnings
cargo fmt --check
scripts/matrix.sh
```

The isolated script creates temporary manifests and symlinks to the actual
resolver/types src directories; it does not edit or replace source or tests.
Only unrelated crates are excluded from this targeted check. Its dependency
resolution uses cached compatible versions, not the full workspace lockfile;
this is NOT a substitute for the required full-workspace checks.
Final outputs: [tests](audit/evidence/resolver-final.txt),
[isolated clippy](audit/evidence/resolver-clippy.txt),
[workspace tests](audit/evidence/workspace-test-followup.txt),
[workspace clippy](audit/evidence/clippy-followup.txt),
[formatting](audit/evidence/fmt-followup.txt),
[matrix](audit/evidence/matrix-table-followup.txt).
Workspace tests/clippy remain UNVERIFIED (tinyvec_macros missing).
Formatting still fails. All six container rows remain UNVERIFIED (Docker absent).

Read-only tracepoint probe:

```text
$ ls -l /sys/kernel/tracing/events/syscalls/sys_enter_execve/format /sys/kernel/tracing/events/syscalls/sys_enter_connect/format
ls: cannot access '/sys/kernel/tracing/events/syscalls/sys_enter_execve/format': Permission denied
ls: cannot access '/sys/kernel/tracing/events/syscalls/sys_enter_connect/format': Permission denied
```

No privilege escalation was attempted. Kernel offsets, actual event capture,
malicious PKGBUILD flagging and installation gating remain UNVERIFIED.

### Remaining resolution/build risks

- Provider compatibility filtering only considers packages selected in earlier
  BFS frontiers. This is not global backtracking; a conflict revealed later
  can still reject a solvable request. No complete solver claim is made.
- AUR builds without --report build artifacts but do not install AUR dependencies
  between builds. A dependency-first plan alone does not make build tools or
  libraries available to later builds. Behavior change needs an explicit design
  choice about safe unmonitored dependency installation/build environments.
- AurPackage has no CheckDepends field; makepkg's check() may require missing
  test tools. This is a source observation, runtime reproduction UNVERIFIED.
- VCS-prefix fix covers HTTP/HTTPS/FTP transports only. SSH/native git transports,
  redirects, IPv6 and relative openat paths still need review.
- Source inspection confirms custom *.db files ARE discovered by OfficialRepos;
  the unresolved issue is honoring pacman.conf enablement/priority/Include,
  not a blanket claim that custom repositories are ignored.
- No installed Archon executable was rebuilt or replaced. These are working-tree
  source fixes, not a verified deployment. Commit author identity and earlier
  design questions are still pending.

## Follow-up: executable missing from shell PATH

User command: `archon install google-chrome` →
`zsh: command not found: archon`. Severity medium; location: user executable
installation/PATH, rather than a Rust source defect. An older 0.1.1 binary
existed in ~/.cargo/bin, which was absent from PATH. ~/.local/bin was
already configured in the user's Zsh PATH.

Status: fixed locally. `cargo build --locked --offline --bin archon` now
succeeds; the dependency cache has changed since earlier attempts. The
current development binary was copied atomically to ~/.local/bin/archon,
without changing shell configuration or installing host system packages.
Zsh resolves that path and `archon --version` reports 0.2.0. Earlier statements
that no binary had been deployed describe the earlier audit state only.

[Raw before/after and runtime checks](audit/evidence/command-path.txt).
A Debian precedence fixture also passes through the installed executable.
The real google-chrome dry run fails reaching AUR with DNS resolution failure;
Chrome resolution/build/install remain UNVERIFIED. No privileged command or
actual package installation was attempted. Full-workspace tests are still
UNVERIFIED: the latest offline attempt is blocked by missing fastrand v2.4.1.
This user-local installation does not make the executable available to sudo's
restricted PATH; monitoring commands must use an explicit executable path.

## User-confirmed Chrome build, 22:07

The user supplied a successful live `archon install google-chrome` resolution
and sandbox build: 181 packages (180 official already current, 1 AUR), Fetch
and Build succeeded, then the CLI printed `Not installed. Re-run with --report`.
This supersedes the earlier environment's DNS failure for that user's run;
it does not establish that our restricted environment can reach AUR.

Read-only verification in this session:

```text
$ pacman -Qip /home/ravi/.cache/archon/build/google-chrome/google-chrome-155.0.8059.84-1-x86_64.pkg.tar.zst
Name            : google-chrome
Version         : 155.0.8059.84-1
Architecture    : x86_64
Install Script  : Yes
Validated By    : None
Signatures      : None
[metadata query exit: 0; selected output fields shown]

$ pacman -Q google-chrome
error: package 'google-chrome' was not found
[exit: 1]
```

Confirmed finding, medium severity: install.rs:262 asks `Proceed with
installation?`, but install.rs:275 dispatches to a build-only AUR path and
returns success without installing the requested package. The deliberate
monitoring policy explains the behavior; the command/prompt remains misleading.
Status: needs-decision. Options presented: require monitoring by default and
fail clearly if unavailable, or allow explicitly confirmed unmonitored installs.
No policy change, privileged invocation, or package installation was performed.
An embedded BPF object exists, but actual privileged monitoring is UNVERIFIED.

## Installation-policy change explicitly requested by the user

The user explicitly requested ordinary `archon install google-chrome` to
complete installation, superseding the prior requirement to choose between
monitoring policies. Default AUR installation now builds in bubblewrap and
hands each successful output to `sudo pacman -U --needed -- <archive>` before
building the next package. Pacman retains its own transaction prompt.
`--report` remains optional; the existing monitored branch still refuses
flagged output. No privileged command was executed by this audit.

Finding: high functional impact, crates/archon/src/commands/install.rs:497.
Root cause: the default install dispatcher called a legacy Phase 2 function
that only built packages, printed `Not installed`, and returned success. It
never invoked pacman, despite the preceding installation prompt. The same
omission left built AUR dependencies unavailable to later builds. Status:
fixed in source and deployed to the user's ~/.local/bin/archon development
binary. Actual privileged package installation is UNVERIFIED.

Failed-first regression:

```sh
python3 scripts/repro-aur-install.py success
```

This full CLI attempt is UNVERIFIED: the environment rejects localhost sockets
with `PermissionError: [Errno 1] Operation not permitted`. Raw failure:
[audit/evidence/aur-install-before.txt](audit/evidence/aur-install-before.txt).
It did not reach Archon's installation logic and is not counted as a bug
reproduction.

The explicit no-network alternative invokes the production function:

```sh
python3 scripts/repro-aur-install.py --direct success
```

[Raw before](audit/evidence/aur-install-direct-before.txt): build succeeds,
`Not installed. Re-run with --report`, exit 0, mocked privileged argv `[]`,
then `AssertionError: successful AUR install must invoke pacman -U exactly once`.

```sh
python3 scripts/repro-aur-install.py --direct
```

[Raw after](audit/evidence/aur-install-direct-after.txt): seven cases pass.
The successful case records argv `pacman -U --needed -- <fixture archive>`.
Cases also cover pacman failure propagation, failed-build blocking, missing
output, detached signature exclusion, ambiguous archives, and installation
of an AUR dependency before its dependent's build. Missing or ambiguous
output fails explicitly; signature files, symlinks and directories are not
selected as package artifacts.

The direct harness copies the actual install.rs module without modification,
appends a small private-function caller, and compiles it against the actual
other CLI modules and existing compiled dependency rlibs. It does not replace
the build/install implementation. Only bwrap and sudo are fake executables.
The fake sudo logs arguments and never executes them. This verifies the
production-function behavior, not complete CLI/AUR RPC dispatch or real
package archives, bwrap isolation, or pacman installation. The harness disables
only dead_code warnings from unused CLI functions, not tests or diagnostics
from the production build.

```sh
cargo build --locked --offline --bin archon
```

Succeeded: `Finished dev profile [unoptimized + debuginfo] target(s) in 1.74s`.
The binary was atomically deployed to ~/.local/bin/archon. Previous executable:
/tmp/archon-before-aur-install-kufftv3d. Shell lookup and `archon --version`
succeed (`archon 0.2.0`). This replaces the implementation without changing
its package version.

```sh
ARCHON_AUDIT_BIN=/home/ravi/.local/bin/archon python3 scripts/repro-aur-install.py dry-run
```

[Raw installed-CLI result](audit/evidence/aur-installed-cli-dry-run.txt):
PASS; fixture official-package plan succeeds; no build artifact or privileged
invocation. This fixture needs no socket. A full AUR CLI dry-run regression
is still UNVERIFIED in the restricted environment.

Required broader checks were rerun:

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --locked --offline -- -D warnings
cargo fmt --check
scripts/matrix.sh
```

[Workspace tests](audit/evidence/aur-install-workspace-test.txt): UNVERIFIED,
missing fastrand v2.4.1. [Clippy](audit/evidence/aur-install-clippy.txt): fails
on existing identical branches in version.rs:284,293.
[Formatting](audit/evidence/aur-install-fmt.txt): fails on existing workspace
diffs. [Matrix](audit/evidence/aur-install-matrix.txt): all six distributions
UNVERIFIED because Docker is absent. No tests were weakened or removed.

Remaining limitations: no global resolver backtracking; incomplete checkdepends
and split-package handling; stale/multiple cached artifacts require cleanup
when the explicit ambiguity error occurs; root-install artifact TOCTOU is not
solved by this change. The modified plan's live installation and live flagged
monitor rejection are UNVERIFIED. The latter branch's gating was not changed.
Commit creation remains blocked by the previously reported missing Git author
identity. No user identity was invented.
