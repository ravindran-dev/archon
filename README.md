# archon

[![Crates.io](https://img.shields.io/crates/v/archon-cli.svg?logo=rust)](https://crates.io/crates/archon-cli)
[![Downloads](https://img.shields.io/crates/d/archon-cli.svg)](https://crates.io/crates/archon-cli)
[![docs.rs](https://img.shields.io/docsrs/archon-cli)](https://docs.rs/archon-cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-171%20passing-brightgreen)](docs/DESIGN.md#testing)
[![Platform](https://img.shields.io/badge/platform-Linux-informational?logo=linux&logoColor=white)](#supported-distros)

**A cross-distro package manager with a security-monitored, AUR-aware core for Arch Linux.**

On Arch, archon resolves dependencies across the official repos *and* the
AUR in one graph, then builds AUR packages inside a bubblewrap sandbox
while an eBPF monitor watches every syscall — a compromised `PKGBUILD`
gets caught and reported instead of silently running as you. On every
other major distro, the same CLI delegates to the native package manager,
so `archon install firefox` just works wherever you are.

archon never reimplements pacman, apt, dnf, or makepkg — it orchestrates
them, and adds what none of them do alone.

## Features

- **Unified dependency resolution** (Arch): official repos + AUR in one
  graph, with `provides=()` virtual packages, version constraints,
  conflict detection, and correct AUR build ordering.
- **Sandboxed AUR builds**: two-stage `makepkg` under bubblewrap — network
  allowed only while fetching sources, fully unshared during the build.
- **eBPF build monitoring** (`--report`): every exec, file write, and
  network connection the build makes is checked against a policy; a clean
  report is required before archon offers to install.
- **Smart search**: results ranked by AUR popularity (what users actually
  install), exact matches pinned first — plus automatic "did you mean"
  suggestions when an exact name isn't found.
- **Cross-distro**: detects the host via `/etc/os-release` and delegates
  to `apt`, `dnf`, `zypper`, `apk`, `nix-env`, `xbps`, or `emerge`.
- **Safe by default**: every system change shows a plan and asks first;
  `--dry-run` is guaranteed side-effect free on every distro.

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
sudo archon install spotify --report   # build under eBPF monitoring, install only if clean
archon remove firefox             # pre-checked removal via pacman -Rs
archon build ./path/to/pkgbuild   # sandbox-build a local PKGBUILD (never installs)
```

Useful flags: `--dry-run` (plan only, no changes), `--yes` (skip archon's
prompt), `--all` (search: show every match instead of the top 15).

## Supported distros

| Family | Detected via | Delegates to |
|---|---|---|
| **Arch** (Manjaro, EndeavourOS, …) | `ID`/`ID_LIKE=arch` | full archon pipeline: resolver + AUR + sandbox + eBPF monitor |
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

All three planted attacks caught; the benign demo package builds with
zero flags. Details in [docs/DESIGN.md](docs/DESIGN.md#demo-walkthrough).

## Development

```sh
cargo build --workspace   # stable toolchain only
cargo test --workspace    # 171 tests, no root or live system required
```

The workspace is nine crates (types, repo parser, AUR client, resolver,
sandbox, eBPF monitor + common ABI, distro backends, CLI). Architecture,
crate map, testing philosophy, the crates.io publishing procedure, and
the design notes behind every non-obvious decision — including the bugs
only live testing could find — are in
**[docs/DESIGN.md](docs/DESIGN.md)**.

## License

[MIT](LICENSE)
