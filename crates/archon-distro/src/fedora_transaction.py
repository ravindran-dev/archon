"""DNF5 transaction bridge, embedded by the CLI and run with Python isolated mode.

Only bounded JSON requests and decisions cross stdin. Native output goes to
stderr; stdout is reserved for the versioned JSON-lines protocol. No caller
supplies a configuration file, install root, command, or executable path.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys

LIMIT = 1024 * 1024
RESERVE = 64 * 1024 * 1024


class BridgeError(Exception):
    def __init__(self, code, message, uncertain=False):
        super().__init__(message)
        self.code = code
        self.uncertain = uncertain


def read_message(stream):
    line = stream.readline(LIMIT + 1)
    if not line or len(line) > LIMIT:
        raise BridgeError("protocol_error", "Missing or oversized protocol message")
    return json.loads(line)


def validate_request(request):
    if set(request) != {"operation", "packages"}:
        raise BridgeError("invalid_request", "Unexpected request fields")
    operation, packages = request["operation"], request["packages"]
    if operation not in {"install", "remove", "upgrade", "reinstall"}:
        raise BridgeError("invalid_request", "Unsupported operation")
    if not isinstance(packages, list) or len(packages) > 256:
        raise BridgeError("invalid_request", "Invalid package targets")
    if (operation == "upgrade") != (len(packages) == 0):
        raise BridgeError("invalid_request", "Upgrade accepts no targets; other operations require targets")
    for package in packages:
        if not isinstance(package, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9+_.:@<>=~\-]{0,255}", package):
            raise BridgeError("invalid_request", "Options, paths and groups are not package targets")


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def identity(package):
    return {"name": package.get_name(), "version": package.get_evr(), "architecture": package.get_arch()}


def policy_fingerprint(base):
    # Include distro config, plugins, variables and RPM policy. No cache/log
    # contents: those legitimately change between prepare and execution.
    roots = [Path(p) for p in ("/etc/dnf", "/etc/yum.repos.d", "/etc/yum.conf", "/etc/rpm", "/etc/pki/rpm-gpg", "/usr/share/dnf5", "/usr/lib/sysimage/libdnf5/system.toml")]
    roots.extend(Path(p) for p in base.get_config().get_reposdir_option().get_value())
    roots.extend(Path(p) for p in base.get_config().get_varsdir_option().get_value())
    return fingerprint_files(roots)


def fingerprint_files(roots):
    contents = {}
    for root in roots:
        paths = sorted(root.rglob("*")) if root.is_dir() else [root]
        for path in paths:
            if path.is_file():
                contents[str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
    return digest(contents)


def normalize_plan(lib, base, transaction, request):
    if transaction.get_transaction_groups() or transaction.get_transaction_environments():
        raise BridgeError("unsupported_transaction", "Group/environment transactions are not yet represented")
    changes = []
    for item in transaction.get_transaction_packages():
        package = item.get_package()
        action = lib.transaction.transaction_item_action_to_string(item.get_action()).lower().replace(" ", "_")
        if action not in {"install", "remove", "upgrade", "downgrade", "reinstall", "replaced", "reason_change"}:
            raise BridgeError("unsupported_transaction", "Unknown native action: " + action)
        inbound = lib.transaction.transaction_item_action_is_inbound(item.get_action())
        checksum = package.get_checksum() if inbound else None
        changes.append({
            "action": action, "package": identity(package),
            "reason": lib.transaction.transaction_item_reason_to_string(item.get_reason()),
            "repository": package.get_repo_id(),
            "download_bytes": int(package.get_download_size()) if inbound else 0,
            "install_bytes": int(package.get_install_size()) if inbound else 0,
            "checksum": (checksum.get_type_str() + ":" + checksum.get_checksum()) if checksum else None,
            "replaces": sorted((identity(p) for p in item.get_replaces()), key=lambda p: json.dumps(p, sort_keys=True)),
            "replaced_by": sorted((identity(p) for p in item.get_replaced_by()), key=lambda p: json.dumps(p, sort_keys=True)),
        })
    changes.sort(key=lambda item: (item["package"]["name"], item["package"]["architecture"], item["package"]["version"], item["action"]))
    installed = lib.rpm.PackageQuery(base)
    installed.filter_installed()
    state = sorted((p.get_nevra(), int(p.get_reason()), int(p.get_rpmdbid())) for p in installed)
    return {
        "schema_version": 1, "backend": "libdnf5", "request": request,
        "changes": changes,
        "download_bytes": sum(p["download_bytes"] for p in changes),
        "install_bytes": sum(p["install_bytes"] for p in changes),
        "system_fingerprint": digest(state), "policy_fingerprint": policy_fingerprint(base),
        "warnings": ["Download bytes are total archive sizes; cache reuse may reduce network transfer.",
                     "Install bytes are a conservative inbound total, not net filesystem growth.",
                     "Native repository and signature policy applies; a plan is not a safety certification."],
    }


def check_disk(plan, cache_path, root_path="/"):
    cache = Path(cache_path)
    while not cache.exists():
        cache = cache.parent
    root = Path(root_path)
    download, installed = plan["download_bytes"], plan["install_bytes"]
    if cache.stat().st_dev == root.stat().st_dev:
        checks = [(root, download + installed)]
    else:
        checks = [(cache, download), (root, installed)]
    for path, required in checks:
        if required and shutil.disk_usage(path).free < required + RESERVE:
            raise BridgeError("insufficient_disk", f"Insufficient free space on {path}; need conservatively {required + RESERVE} bytes")


def resolve(lib, request, write=False):
    base = lib.base.Base()
    base.load_config()
    # Native configuration and libdnf5 plugins are retained. No signature,
    # repository-priority, protected-package or locking settings are weakened.
    base.setup()
    sack = base.get_repo_sack()
    sack.create_repos_from_system_configuration()
    # Newer DNF5 exposes the system-repository lock explicitly. A read lock
    # cannot be promoted, so an execution session takes WRITE before loading
    # RPMDB and retains it for the entire resolved transaction. Older DNF5
    # versions retain their own locking/system-change checks in run().
    if hasattr(base, "lock_system_repo"):
        access = lib.utils.LockAccess_WRITE if write else lib.utils.LockAccess_READ
        if not base.lock_system_repo(access, lib.utils.LockBlocking_NON_BLOCKING):
            raise BridgeError("native_locked", "Native system repository is locked by another package operation")
    sack.load_repos()
    goal = lib.base.Goal(base)
    if request["operation"] == "upgrade":
        goal.add_rpm_upgrade()
    else:
        add = getattr(goal, "add_rpm_" + request["operation"])
        for package in request["packages"]:
            add(package)
    transaction = goal.resolve()
    if transaction.get_problems() != lib.base.GoalProblem_NO_PROBLEM:
        raise BridgeError("resolve_failed", "\n".join(transaction.get_resolve_logs_as_strings()))
    return base, transaction


def execute(lib, base, transaction, plan, checkpoint):
    check_disk(plan, base.get_config().get_cachedir_option().get_value())
    checkpoint("downloading")
    transaction.download()
    checkpoint("verifying")
    if not transaction.check_gpg_signatures():
        raise BridgeError("signature_failed", "Native signature verification failed; no signature settings were overridden")
    if policy_fingerprint(base) != plan["policy_fingerprint"]:
        raise BridgeError("stale_plan", "Native configuration changed after resolution; prepare a new plan")
    # The parent must fsync a committing checkpoint before acknowledging this.
    checkpoint("committing")
    result = transaction.run()
    result_type = lib.base.Transaction
    if result != result_type.TransactionRunResult_SUCCESS:
        safe = {getattr(result_type, "TransactionRunResult_" + name) for name in (
            "ERROR_CHECK", "ERROR_GPG_CHECK", "ERROR_LOCK", "ERROR_RESOLVE", "ERROR_RERUN", "ERROR_SYSTEM_CHANGED")}
        raise BridgeError("native_transaction_failed", "Native transaction result: " + str(result), result not in safe)


def serve(stream, emit, lib=None):
    phase = "resolving"
    try:
        message = read_message(stream)
        if set(message) != {"schema_version", "mode", "request"} or message["schema_version"] != 1 or message["mode"] not in {"plan", "apply"}:
            raise BridgeError("protocol_error", "Unsupported transaction protocol")
        request, mode = message["request"], message["mode"]
        validate_request(request)
        if mode == "apply" and os.geteuid() != 0:
            raise BridgeError("privilege_required", "Native transaction execution requires administrator authorization")
        if Path("/run/ostree-booted").exists():
            raise BridgeError("unsupported_host", "Use an rpm-ostree deployment adapter on this host")
        if lib is None:
            try:
                import libdnf5 as lib
            except ImportError as error:
                raise BridgeError("missing_native_api", "Install Fedora's python3-libdnf5 package to use native transactions") from error
        emit({"event": "ready", "pid": os.getpid()})
        if read_message(stream) != {"continue": True}:
            raise BridgeError("cancelled", "Parent did not acknowledge session")
        base, transaction = resolve(lib, request, write=mode == "apply")
        plan = normalize_plan(lib, base, transaction, request)
        emit({"event": "plan", "plan": plan})
        if mode == "plan":
            emit({"event": "complete"})
            return 0
        if read_message(stream) != {"approve": True}:
            emit({"event": "cancelled"})
            return 0

        def checkpoint(next_phase):
            nonlocal phase
            emit({"event": "checkpoint", "phase": next_phase})
            if read_message(stream) != {"continue": True}:
                raise BridgeError("cancelled", "Parent disconnected or did not persist checkpoint")
            phase = next_phase

        transaction.set_description("Archon " + request["operation"] + " " + " ".join(request["packages"]))
        user_id = int(os.environ.get("SUDO_UID", os.getuid()))
        if not 0 <= user_id <= 2**32 - 1:
            raise BridgeError("invalid_identity", "Invalid native transaction user identity")
        transaction.set_user_id(user_id)
        execute(lib, base, transaction, plan, checkpoint)
        emit({"event": "complete"})
        return 0
    except Exception as error:
        emit({"event": "error", "code": getattr(error, "code", "native_failure"),
              "message": str(error), "uncertain": getattr(error, "uncertain", phase == "committing")})
        return 1


if __name__ == "__main__":
    # Redirect even C/C++ writes, not just Python prints. The protocol retains
    # the original pipe on a separate descriptor.
    protocol = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
    decisions = os.fdopen(os.dup(sys.stdin.fileno()), "r", encoding="utf-8")
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    # Native prompts and RPM scriptlets must never consume parent protocol
    # decisions. The private duplicate is not inherited across exec; native
    # stdin sees EOF, including for unapproved key prompts.
    null_input = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null_input, sys.stdin.fileno())
    os.close(null_input)

    def emit(message):
        protocol.write(json.dumps({"schema_version": 1, **message}, sort_keys=True) + "\n")
        protocol.flush()

    sys.exit(serve(decisions, emit))
