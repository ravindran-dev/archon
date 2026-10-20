# archon

[![Crates.io](https://img.shields.io/crates/v/archon-cli.svg?logo=rust)](https://crates.io/crates/archon-cli)
[![Downloads](https://img.shields.io/crates/d/archon-cli.svg)](https://crates.io/crates/archon-cli)
[![docs.rs](https://img.shields.io/docsrs/archon-cli)](https://docs.rs/archon-cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Audit](https://img.shields.io/badge/audit-incomplete-yellow)](AUDIT.md)
[![Platform](https://img.shields.io/badge/platform-Linux-informational?logo=linux&logoColor=white)](#supported-distros)

**A cross-distro package manager with a security-monitored, AUR-aware core for Arch Linux.**

On Arch, archon resolves dependencies across the official repos and AUR,
builds AUR packages in a bubblewrap sandbox, and installs the resulting
archives through pacman. Optional eBPF monitoring is available with `--report`.
Other supported distro families delegate installation to their native package
manager. Version 0.2.0 is experimental; unresolved security findings and
verification limits are documented in [AUDIT.md](AUDIT.md).

## Features

- **Unified dependency resolution** (Arch): official repos + AUR in one
  graph, with `provides=()` virtual packages, version constraints,
  conflict detection, bounded backtracking, check dependencies, and
  dependency-first AUR builds grouped by PackageBase.
- **Sandboxed AUR builds**: two-stage `makepkg` under bubblewrap — network
  allowed only while fetching sources, fully unshared during the build.
- **eBPF build monitoring** (`--report`): policy checks for captured exec,
  file-open and connection events. Flagged and zero-event reports block
  installation. Live coverage and root-path safety remain unverified.
- **Smart search**: results ranked by AUR popularity (what users actually
  install), exact matches pinned first — plus automatic "did you mean"
  suggestions when an exact name isn't found.
- **Cross-distro**: detects the host via `/etc/os-release` and delegates
  to `apt`, `dnf`, `zypper`, `apk`, `nix-env`, `xbps`, or `emerge`.
- **Plans and confirmation**: inspect a request with `--dry-run` before
  installation. Cross-distro end-to-end behavior remains unverified.

## Installation

```sh
cargo install archon-cli
```

This installs a binary named `archon` into `~/.cargo/bin` (make sure it's
on your `PATH`). The package is `archon-cli` because the name `archon` was
taken on crates.io — the command you type is still `archon`.

To use `sudo archon ... --report` (root is required to load the eBPF
monitor), symlink the binary onto root's `secure_path`:

```sh
sudo ln -sf "$(which archon)" /usr/local/bin/archon
```

**Requirements**: Rust stable (that's all — the compiled eBPF monitor
ships embedded in the binary). On Arch, the sandbox additionally needs
`bubblewrap` (`sudo pacman -S bubblewrap`) and cgroups v2 (the default on
any current install). Other distros need nothing beyond their own package
manager.

**From source:**

```sh
git clone https://github.com/ravindran-dev/archon
cd archon
cargo install --path crates/archon --locked
```

## Quick start

```sh
archon search chrome              # ranked search: official repos + AUR
archon plan firefox               # show the full install plan, change nothing
archon install firefox            # resolve, confirm, install
archon install google-chrome      # sandbox-build AUR package, then install via pacman
sudo archon install spotify --report   # build under eBPF monitoring, install only if clean
archon remove firefox             # pre-checked removal via pacman -Rs
archon build ./path/to/pkgbuild   # sandbox-build a local PKGBUILD (never installs)
```

Useful flags: `--dry-run` (plan only, no changes), `--yes` (skip archon's
prompt), `--all` (search: show every match instead of the top 15).

## Supported distros

| Family | Detected via | Delegates to |
|---|---|---|
| **Arch** (Manjaro, EndeavourOS, …) | `ID`/`ID_LIKE=arch` | resolver + AUR + sandbox; optional eBPF monitor |
| Debian / Ubuntu (Mint, Pop!_OS, …) | `debian`/`ubuntu` | `apt` |
| Fedora / RHEL (Rocky, Alma, …) | `fedora`/`rhel` | `dnf` |
| openSUSE / SLE | `suse`/`opensuse` | `zypper` |
| Alpine | `alpine` | `apk` |
| NixOS | `nixos` | `nix-env` (no sudo — installs to your user profile) |
| Void | `void` | `xbps` |
| Gentoo | `gentoo` | `emerge` |

Off Arch, the native tool owns dependency resolution and its own
confirmation — archon provides detection and one consistent CLI. `--yes`
maps to each tool's own assume-yes flag; `--report` isn't available off
Arch (it needs the PKGBUILD build-recipe model) and says so instead of
silently ignoring the flag. Anything unlisted gets a clear error naming
what was detected. Adding a backend is a small trait impl — see
[docs/DESIGN.md](docs/DESIGN.md#cross-distro-support).

## Security monitoring in action

The repo ships a deliberately malicious demo package. Building it under
the monitor:

```sh
sudo archon build ./demo/archon-demo-evil --report
```

```
Build report for archon-demo-evil
  3022 syscall event(s) observed
  9 event(s) FLAGGED:
    [!] wrote outside $srcdir/$pkgdir: /var/tmp/archon-evil-marker
    [!] wrote outside $srcdir/$pkgdir: /tmp/archon-evil-marker
    [!] connected to undeclared address: 1.1.1.1:80

==> build FLAGGED — treat the output as untrusted.
```

The example above is a pre-audit demonstration, not a result reproduced by
this audit. Live benign/malicious build behavior remains UNVERIFIED. Details in [docs/DESIGN.md](docs/DESIGN.md#demo-walkthrough).

## Development

```sh
cargo build --workspace   # stable toolchain only
cargo test --workspace    # HTTP mocks need localhost sockets; sandbox tests need bwrap support
```

The workspace is nine crates (types, repo parser, AUR client, resolver,
sandbox, eBPF monitor + common ABI, distro backends, CLI). Architecture,
crate map, testing philosophy, the crates.io publishing procedure, and
the design notes behind every non-obvious decision — including the bugs
only live testing could find — are in
**[docs/DESIGN.md](docs/DESIGN.md)**.

## Known Limitations

Archon 0.2.0 remains experimental. See [AUDIT.md](AUDIT.md) for failed-first
regressions, raw verification logs, and unresolved security findings. The
empty-database fixture and all-target clippy failures reported on 2026-10-20
are fixed; the full locked workspace suite and live privileged/container
checks still need successful runs in an environment that permits localhost
test servers, eBPF/cgroups, Docker, and administrator elevation.

- Arch resolution now backtracks across alternatives and providers, including
  late conflicts. Search is bounded (100,000 steps by default); exhaustion
  returns an explicit error. This does not guarantee every solvable request
  completes within the limit.
- AUR check dependencies are included, and selected split outputs sharing a
  PackageBase build together. Dynamic PKGBUILD metadata, VCS freshness, stale
  clones, and changing upstream dependencies still need live coverage.
- Non-Arch installs delegate to native package managers. The experimental
  AptLists parser/resolver is not used for these installs; Debian multiarch
  qualifiers, Breaks, and Replaces are not fully modeled in that API.
- Local `archon build` requires the Arch makepkg toolchain. A managed Arch
  container backend and a clearer non-Arch preflight remain unresolved.
- Ordinary AUR installs build in bwrap and then install through native pacman.
  Before the first AUR install, run `archon setup-helper` once; this requests
  administrator authorization to copy the companion `archon-install-helper`
  to `/usr/local/libexec` as root-owned mode 0755. It does not set the helper
  setuid. The receiver accepts only bounded package/signature bytes on stdin,
  stages them in root-private storage, and passes staged archives to pacman
  without changing `/etc/pacman.conf`; pacman remains responsible for native
  signature verification and transaction confirmation. `LocalFileSigLevel`
  requirements are supported when valid adjacent signatures are provided.
  eBPF is optional through `--report`. Zero-event and flagged reports block
  installation. Live tracepoint compatibility, event loss, malicious builds,
  and privileged installation are UNVERIFIED in this audit environment.
- PGP imports now execute inside bwrap as the build user, with strict
  fingerprints and GPG options disabled. The parent never modifies the build
  keyring. Real unprivileged bwrap/GPG preparation is tested; privileged user
  switching and successful signature verification still need end-to-end checks.
- Archive identity is checked with pacman; ambiguous outputs fail explicitly.
  The archive and adjacent signature are frozen before transfer, then staged
  privately by the root-owned receiver. Hermetic protocol tests pass; the
  real privileged receiver and native signature enforcement remain UNVERIFIED
  here. Custom pacman.conf
  repository priority, cache concurrency,
  and all package relationship combinations have not been exhaustively audited.
- ALPM comparison matched native vercmp for 900 tested pairs. Native dpkg parity
  remains UNVERIFIED here because dpkg is absent. Cross-backend comparisons now
  return typed errors instead of panicking.
- Build, strict clippy, and formatting pass. Full workspace tests are blocked
  by an unavailable fastrand dependency and restricted network access; HTTP
  client tests additionally cannot bind localhost sockets in this environment.
  A real benign sandbox build reaches Fetch but Build fails because the
  environment denies NETLINK_ROUTE sockets; network isolation remains enabled.
  The six-distro Docker matrix is supplied but UNVERIFIED because Docker is
  absent. Run `scripts/matrix.sh` and optionally `scripts/matrix.sh --install`
  on a Docker host; transactions occur only inside disposable containers.

## License

[MIT](LICENSE)
