# ArkaOS integration

ArkaOS retains Fedora's ID. Archon checks its release marker and os-release
identity before routing; native DNF mutations are unavailable. `archon
capabilities` emits JSON for ArkaOS. No image mutation command is enabled.

`archon-distro::image` defines serializable plans and validated state transitions.
Plans bind base and metadata SHA-256 digests, an approved source, exact package
identifiers and a maximum one-hour lifetime. Lists must be sorted and unique.
Validation does not resolve dependencies, certify NEVRAs, authorize execution,
or verify signatures. A future builder must resolve and attest the full closure.
State transitions cannot skip from a build or staged image to success.

ArkaOS owns the separate org.arka.Archon1 read-only service, image signing,
build workers, deployment authorization and recovery. Flatpak remains the
application installation path. No AUR or arbitrary recipe executes in the OS
service. There is no implemented builder or privileged deployment broker yet.

Validation: cargo test --offline --locked -p archon-distro (34 passed);
cargo build --offline --locked --release -p archon-cli --bin archon;
release --help, ArkaOS capabilities and install --dry-run refusal.
Workspace testing is blocked by sandbox socket permissions in five AUR tests.
Container and boot/rollback tests are still required before release.
