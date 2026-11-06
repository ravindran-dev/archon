# Nizho integration development

Nizho is an immutable bootc OS. The Nizho release marker and branded os-release
must never select the live DNF backend. Nizho keeps image deployment, signing,
boot verification and rollback; Archon owns package identities and native backend
logic. Existing distro implementations remain unchanged.

The development `archon-distro::flatpak` adapter embeds a bounded protocol bridge
to libflatpak via Python GObject. libflatpak owns discovery, repositories,
dependency closure, native state and signature enforcement. The bridge's `ready`
callback exports an abort-before-apply plan. Commit independently resolves and
compares operations, installed state and repository/key fingerprints. It refuses
new remotes, disabled signatures, stale plans and expired commits. Cancellation
uses Gio.Cancellable, not a shell command.

The bridge itself is not an authorization service. Nizho's root system service
must bind plans to the real D-Bus caller, authorize through polkit, persist state
and prevent replay/conflicts before invoking commit. Debug isolated-installation
arguments are for actual disposable repository tests, not a production bypass.

On a Nizho system shipping the matching service, normal Archon commands use the
system Flatpak backend through `org.nizho.Archon1.Manager`:

```sh
archon capabilities
archon search Calculator
archon list --json
archon info app/org.gnome.Calculator/x86_64/stable
archon plan app/org.gnome.Calculator/x86_64/stable
archon install app/org.gnome.Calculator/x86_64/stable
archon remove app/org.gnome.Calculator/x86_64/stable
archon upgrade --dry-run
archon history
```

Use actual refs from search; these commands do not assume an application is in a
configured remote. Ambiguous short names are rejected. `--yes` does not bypass
polkit. The CLI is unprivileged and executes no privileged package operation.
`archon update` is the generic self-update alias, but Nizho rejects that operation
for its OS-managed executable. Archon updates with the OS image; application
upgrades use `archon upgrade`. System RPM image operations remain unsupported until Nizho has
a validated build/sign/deployment broker. Do not use live DNF as a fallback.

Native validation:

```sh
cargo test --locked --workspace -- --test-threads=2
python3 scripts/test-flatpak-native.py
```

The Python test creates a signed local Flatpak app/runtime repository and a
separate temporary installation. It validates no-install planning, expiry,
cancellation, signed install/update, stale-state rejection, removal and unsigned
artifact/policy rejection. It does not modify the host package installation and
does not certify Nizho VM boot or polkit authentication. Nizho's source/native/UI
suite additionally exercises the actual D-Bus service and GTK widgets.
