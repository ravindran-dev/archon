# Native plans, execution, recovery and release validation

The prepared transaction path is implemented first for conventional Fedora
hosts using the `libdnf5` Python bindings. It is opt-in. Existing install/remove
and lifecycle wrappers remain available, and other adapters do not inherit
Fedora behavior. This is an implementation awaiting real native validation,
not a declaration that Archon is production-ready.

## Use

Fedora needs the distro-provided `python3-libdnf5` package and its native RPM/DNF
configuration. Native transactions on rpm-ostree hosts are refused. Run planning
as your normal user; execution obtains administrator authorization through sudo.

```sh
archon plan --native firefox
# Equivalent explicit operation:
archon transaction plan install firefox
archon transaction plan remove firefox
archon transaction plan upgrade
archon transaction plan reinstall firefox

# Review the saved transaction ID, then execute that plan:
archon transaction show TRANSACTION_ID
archon transaction apply TRANSACTION_ID

archon transaction list
archon transaction recover TRANSACTION_ID
```

Planning can refresh native metadata caches and writes an Archon recovery
record; it does not download package payloads or commit a package transaction.
The original `--dry-run` is still a command preview on non-Arch systems.
It is deliberately separate from accurate native resolution.

## Accurate plans

The adapter asks the native solver to resolve install/remove/upgrade/reinstall.
It does not parse terminal tables or treat `--assumeno` as a successful plan.
The normalized plan includes:

- Every native package action: install, remove, upgrade, downgrade, reinstall,
  replaced package, or install-reason change.
- Package name, native version including epoch, architecture and repository.
- Native install reason, including user requests and dependencies.
- Replacement relationships in both directions, preserving old and new versions.
- Package archive sizes, repository checksums and conservative installed sizes.
- Installed-state and native-policy fingerprints.

Archive bytes are total package sizes; cache reuse can reduce network transfer.
Installed bytes are the sum of inbound sizes, not net disk usage or a guarantee
that every filesystem has enough space. Native RPM checks remain authoritative.
Groups, environments, local RPM paths and unsupported native actions fail
explicitly until they have complete plan representations.

On apply, the privileged native session resolves the saved request again and
compares the full plan before asking for confirmation. Changed installed
packages/reasons/RPMDB identities, package checksums, repositories, or policy
refuse execution with `stale_plan`. Prepare and review a fresh plan to proceed;
`--yes` never overrides stale-plan refusal.

Once approved, that same in-memory native transaction is downloaded, signature
checked and run. No separate DNF CLI invocation silently resolves a third plan.

## Native correctness and privilege boundaries

The adapter loads native configuration and libdnf5 plugins, repositories,
variables and protected-package policy. It supplies no `--nogpgcheck`, force,
dependency-ignore, install-root, arbitrary config-path, or lock-removal override.
Trust remains the system administrator's native policy: configurations which
disable a signature check are not silently replaced with Archon policy.

New DNF5 versions expose system-repository locks. Planning takes a native read
lock; execution takes a nonblocking write lock before loading RPMDB, held for
the lifetime of the resolved transaction. Contention returns `native_locked`.
Older DNF5 versions retain their native transaction locking and system-change
checks in `run()`. The library also rechecks signatures/system state during
execution. DNF4's Python API is not implemented in this prepared path.

The CLI and journal stay under the invoking user. Only the embedded bridge runs
as administrator. Python uses isolated mode, an absolute interpreter path and
`/` as cwd: user site packages, PYTHONPATH and local modules cannot replace
native imports. The bridge accepts bounded JSON requests, not commands,
executable paths, arbitrary configuration, or mutable helper source files.
The original user's identity is supplied to native history when invoked via sudo.

An accurate plan does not certify package contents or script safety. Native
scriptlets retain normal native installation privileges. DNF command-line-only
plugins and custom command extensions are not claimed to have API parity.

## Durable recovery

Records are stored under `$XDG_STATE_HOME/archon/transactions`, or
`~/.local/state/archon/transactions`. The final directory must be owned by the
invoking user with mode 0700. Directory traversal uses descriptor-based
operations without following symlinks. Files reject symlinks, nonregular files,
hard links, unexpected owners and group/other permissions.

Journal updates write a private temporary file, fsync it, atomically rename it,
and fsync the containing directory. A per-user workflow lock prevents concurrent
Archon transaction writers, in addition to the native engine's own locks.

The bridge pauses at each checkpoint until the CLI acknowledges a durable
record:

```text
planned → resolving → awaiting_confirmation → downloading → verifying
        → committing → completed
```

The bridge cannot start commit until the committing checkpoint has been saved.
Approval refusal cancels before work. Policy/signature/disk/native-lock failures
before commit are retryable only through native plan revalidation. Native logs
are drained into a private per-transaction log, bounded at 4 MiB; output beyond
that bound is discarded so a verbose subprocess cannot exhaust memory/disk.

If the coordinator disappears, recovery checks both coordinator and helper
identity using PID, process start time and boot ID. A live helper blocks recovery.
An interrupted commit becomes `uncertain`; it cannot be replayed with apply.
Completed transactions cannot be replayed either. Inspect `archon check`,
`archon history`, the record and native script/package state, then prepare a new
plan after resolving native problems. Archon never guesses that matching file
versions prove successful script completion, deletes native locks, or promises
automatic rollback on filesystems/deployment models without such support.

No automatic native repair command is run: appropriate repair depends on the
native failure. In particular, generic recovery must not rebuild RPMDB or undo
transactions without diagnosing the underlying problem.

## JSON automation

Transaction commands accept `--json` and emit JSON Lines. Every record includes
`schema_version: 1` and `event`. Events include `progress`, `plan`, `transaction`,
`transactions`, `complete`, `cancelled`, `recovery`, and `error`. Plans/package
identities are typed and native versions remain opaque strings.

```sh
archon transaction plan install firefox --json
archon transaction apply TRANSACTION_ID --yes --json
archon transaction recover TRANSACTION_ID --json
```

JSON apply requires `--yes`, after reviewing the saved plan, so human confirmation
prompts cannot corrupt the event stream. Native subprocess output is separated
from the protocol. Native prompts/scriptlets receive EOF rather than protocol
input; unknown key prompts cannot consume approval messages.
Errors are versioned objects with stable bridge codes such
as `resolve_failed`, `stale_plan`, `signature_failed`, `native_locked`, and
`insufficient_disk`; parse/CLI failures also return structured errors and nonzero
exit status. Existing `list --json` keeps its inventory-array format for
compatibility, while its failures use the structured error envelope.

## Verification and release gate

Hermetic coverage includes native action/reason normalization, replacement/epoch
preservation, stale plan refusal, checkpoint order, signature rejection, native
lock failure, configuration changes, disk exhaustion, corrupt/hardlinked state,
process identity, cancellation and missing completion after commit. Python mocks
and Rust subprocess fixtures are explicitly not evidence of live Fedora behavior.

The disposable container matrix now includes real Fedora DNF5 preparation and
reinstallation through the bridge, as well as existing native distro scenarios.
It requires Docker and is configured in CI; it has not run in this agent session.

`scripts/fedora-vm-validation.py` supplies a real Fedora VM harness for:

1. Building/signing fixture RPMs, installing dependency packages, and upgrading
   a fixture with native version checks.
2. A missing-package native resolution failure.
3. An unsigned package rejected under fixture `gpgcheck=1`.
4. Insufficient cache space on a constrained tmpfs, with no package installation.
5. A real interrupted RPM commit across reboot, followed by uncertain recovery
   and refusal to replay the transaction.

Use only a disposable Fedora VM with a working sudo/native toolchain. Provision
`python3-libdnf5`, RPM build/signing tools, GnuPG and createrepo_c. Build the exact
commit being tested and make the Archon binary available in the guest.

```sh
sudo python3 scripts/fedora-vm-validation.py \
  --binary /absolute/path/to/archon --commit FULL_TESTED_COMMIT_SHA \
  --acknowledge-disposable-vm
# The harness exits 75 with a real RPM post-script running.
# Reboot the disposable VM immediately, then run:
sudo python3 scripts/fedora-vm-validation.py \
  --binary /absolute/path/to/archon --commit FULL_TESTED_COMMIT_SHA \
  --acknowledge-disposable-vm --resume
```

The harness modifies guest package/repository/key state and uses a temporary
cache mount. It refuses host/container execution and existing fixture state;
restore a clean snapshot for another run. Its scenarios and API compatibility
remain UNVERIFIED until executed successfully on an actual guest. This harness
does not prove complete OS release upgrades or every filesystem/recovery case.

After successful resume, preserve `/var/lib/archon-vm-validation/evidence.json`
and all scenario logs. Submit them as a `production-vm-evidence` artifact for
the exact release commit. Other distro families need corresponding real VM
evidence; their adapter transaction implementations are still separate work.

`scripts/check-release-evidence.py` refuses a dirty release checkout, wrong
commit, missing distro/scenario, fixture/container evidence, unchanged boot ID,
nonzero scenario result, missing raw logs, out-of-directory log paths and checksum
mismatches. `.github/workflows/release-readiness.yml` downloads that artifact and
requires VM evidence for all eight advertised families. It is an evidence
consistency gate requiring reviewed VM reports, not independent attestation that
a report's author actually performed the tests. No publishing workflow bypass
or production-ready label has been added.

Native API references: [DNF5 session setup](https://dnf5.readthedocs.io/en/latest/tutorial/bindings/python3/session.html),
[transaction APIs and locking](https://dnf5.readthedocs.io/en/latest/api/python/libdnf5_base.html),
[transaction item metadata](https://dnf5.readthedocs.io/en/latest/api/python/libdnf5_transaction.html).

## Local verification results

229 selected Rust tests and 17 Python tests passed: 160 core-crate tests,
37 CLI unit tests, 6 helper tests, 12 adapter integration tests, 10 existing
non-Arch CLI regressions, 4 AUR model tests, 13 bridge/fault tests and 4 evidence
gate tests. Formatting, strict all-target workspace clippy, shell syntax and
Python harness syntax checks passed. A deliberate missing-evidence invocation
returned `release_ready: false` and a nonzero exit status.

The complete socket-dependent workspace suite was not rerun. No real Fedora
API transaction, container matrix or VM reboot test ran here: Docker/Podman and
a disposable Fedora VM are unavailable. This sandbox remaps system directory
ownership, which the journal correctly rejects as an untrusted ancestor; journal
unit tests start from held private fixture-directory descriptors while preserving
the production trust checks. No host package state was changed by this work.
