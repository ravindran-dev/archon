# Cross-distro development contract

Archon's goal is a consistent package-management experience across Linux
distributions, with deep integration into each native package engine. Fedora,
Debian and other families are first-class development targets. The existing
Arch/AUR pipeline is one specialized capability within that architecture.

## Module boundaries

```text
CLI commands
  ├─ install/remove/search: existing dispatch and Arch build workflow
  └─ native lifecycle dispatcher: commands/native.rs
       └─ archon-distro::Backend
            ├─ backend/arch.rs
            ├─ backend/debian.rs
            ├─ backend/fedora.rs
            ├─ backend/suse.rs
            ├─ backend/alpine.rs
            ├─ backend/nix.rs
            ├─ backend/void.rs
            └─ backend/gentoo.rs
```

`backend.rs` defines operations and the adapter interface. Each adapter owns
native arguments, optional capabilities, privilege requirements, scope and host
restrictions. `backend/rpm.rs` contains the RPM inventory protocol shared by
Fedora and openSUSE, not distro-wide transaction policy. The native dispatcher
does not contain DNF/pacman flags or fallback to another backend after failure.

An adapter opts into a new operation explicitly through `operation_argv`.
Unimplemented operations return `None` and produce a clear CLI error. Native
execution uses an argument vector, not a shell. Queries run without sudo except
database checks whose adapter explicitly requires it. Mutation commands have
Archon confirmation; native managers may also request confirmation. `--yes`
skips Archon's prompt and supplies native confirmation flags where available.
`--dry-run` prints the invocation without launching it or acquiring privileges.

## Implemented capability matrix

This table describes code support, not real-system certification. Existing
install/remove/search support remains available on all eight families.

| Operation | Arch | Debian | Fedora | SUSE | Alpine | NixOS | Void | Gentoo |
|---|---|---|---|---|---|---|---|---|
| upgrade | yes | yes | yes | yes | yes | profile | yes | world |
| refresh | use upgrade | yes | yes | yes | yes | channels | yes | yes |
| info | yes | yes | yes | yes | yes | profile source | yes | build info |
| list | yes | yes | yes | yes | yes | profile | yes | qlist |
| reinstall | yes | yes | yes | yes | yes | — | yes | yes |
| repos | — | policy | yes | yes | — | channels | yes | yes |
| check | yes | — | yes | — | — | — | yes | — |
| history | — | — | yes | — | — | — | — | — |
| provides | — | — | yes | yes | — | — | — | — |
| list --json | — | — | RPM | RPM | — | — | — | — |

`upgrade` takes no package arguments. It is an installed-package upgrade within
the native scope, not an OS release upgrade. Arch uses `pacman -Syu`; it does not
yet rebuild/update AUR packages. Debian uses `apt upgrade`, which can keep back
upgrades requiring removals, and requires separately refreshed metadata. SUSE
uses `zypper update`; Tumbleweed distribution synchronization with `dup` needs
a future flavor-aware adapter. Nix upgrades user profiles, not the declarative
OS configuration. Gentoo lists packages with optional `portage-utils`/`qlist`.

## Fedora integration

The Fedora adapter uses commands common to DNF4 and DNF5; `repolist` remains a
DNF5 compatibility alias. Installed package queries use RPM independently of
DNF's human-readable output.

```sh
archon capabilities
archon refresh
archon upgrade --dry-run
archon upgrade
archon info firefox
archon list
archon list --json
archon reinstall firefox
archon repos
archon check
archon history
archon provides '*/libexample.so'
```

Structured inventory includes `name`, native `version` (epoch:version-release),
and `architecture`. Multiple architectures and parallel package versions are
retained. Records are sorted deterministically; malformed native records and
failed queries return errors rather than incomplete successful inventories.
Successful JSON output contains no status banners and requires no sudo.

If `/run/ostree-booted` exists, the Fedora adapter refuses install, remove,
upgrade, refresh and reinstall. This prevents routing image-based hosts through
ordinary DNF mutations. A dedicated rpm-ostree/bootc adapter is still needed;
this check does not claim to detect every immutable deployment model.

No RPM packages are sent through the Arch resolver, PKGBUILD pipeline, version
comparator or AUR. `--report` remains an Arch build capability. Fedora source
builds, COPR, advisory/security updates, release upgrades, structured native
transaction plans, and journal/recovery support remain future work.

## Regression rules

1. Put native flags and policies in the corresponding adapter module. Shared
   code should operate on capabilities and typed requests.
2. Test all existing families whenever the adapter contract or dispatcher
   changes. Test shared protocol changes against every consumer.
3. Run fixture tests with explicit `os-release` data and executable stubs.
   Their PATH contains only stubs so no test can fall back to host tools.
4. Verify command previews launch nothing, read queries do not escalate,
   Nix profile mutations do not use sudo, cancellation prevents execution,
   and native errors never trigger another adapter.
5. Preserve original install/remove/search behavior unless a change is
   intentional and covered by regressions. Fedora-specific restrictions must
   have tests showing that other adapters retain their behavior.
6. Establish real distro evidence separately from fixture tests. A capability
   table or successful command preview does not establish transaction success.

`crates/archon/tests/native.rs` covers the new dispatch paths across all eight
families with stubs. The original CLI tests continue to cover existing behavior.
`.github/workflows/ci.yml` runs workspace checks and the existing six-distro
container matrix on pushes and PRs. The container scenarios exercise jq
install/info/list/remove and real RPM inventory on Fedora/openSUSE, as well as
lifecycle command previews. NixOS, Void and Gentoo still need real native test
environments; privileged builds and monitoring require VM tests.

## Next depth milestones

The first prepared Fedora transaction implementation now exists; see
[NATIVE-TRANSACTIONS.md](NATIVE-TRANSACTIONS.md) for accurate planning, checkpointed
execution, JSON and release-gate behavior. It uses DNF5 bindings, remains opt-in
pending real Fedora validation, and does not change other adapters.

The shared target contract should grow to cover typed package identity,
installed state, repositories, transaction actions and observation/recovery
status while retaining native version semantics. Each adapter implements that
contract through its engine's supported APIs or stable machine interfaces.

For Fedora, the next milestone is a DNF4/DNF5-aware transaction adapter that can
prepare and inspect an exact transaction, detect state changes, and commit or
reconfirm it. Keep DNF exit statuses, repository trust, install reasons,
replacements, advisory metadata and native locking intact. Extend structured
inventory and transaction support to the other families independently, with
explicit unsupported capabilities while work is in progress.

The production blockers in [PRODUCTION-READINESS.md](PRODUCTION-READINESS.md)
still apply to privileged builds, monitoring completeness and recovery. The
new lifecycle interface addresses command coverage and isolation; it does not
establish production readiness or deep transaction parity by itself.

## Local verification of this implementation

214 selected tests passed: 159 core-crate tests, 25 CLI unit tests, 6 helper
tests, 10 new adapter integration tests, 10 existing non-Arch CLI regressions,
and 4 AUR model tests. Strict workspace/all-target clippy, formatting and shell
syntax checks passed. A source comparison also confirmed that all original
adapter methods were preserved during the module split.

The complete workspace suite and real container/VM matrix were not run in this
session. Docker/Podman are unavailable and HTTP mock tests need localhost socket
access restricted by this environment. CI configuration was added but has not
been executed here. No host package installation, removal, upgrade or refresh
was performed; native execution tests resolve only fixture executables.

Native references: [DNF](https://dnf.readthedocs.io/en/latest/command_ref.html),
[DNF5 compatibility](https://dnf5.readthedocs.io/en/latest/changes_from_dnf4.7.html),
[pacman](https://man.archlinux.org/man/pacman.8.en),
[APT](https://manpages.debian.org/bookworm/apt/apt.8.en.html),
[zypper](https://manpages.opensuse.org/Tumbleweed/zypper/zypper.8.en.html),
[XBPS](https://man.voidlinux.org/xbps-install.1),
[Nix profiles](https://nix.dev/manual/nix/2.28/command-ref/nix-env/upgrade.html),
[Portage](https://dev.gentoo.org/~zmedico/portage/doc/man/emerge.1.html),
[apk](https://github.com/alpinelinux/apk-tools/tree/master/doc).
