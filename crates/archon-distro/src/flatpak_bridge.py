#!/usr/bin/python3
"""Archon libflatpak protocol v1. Fixed operations, no shell, native database.

Production always opens the system installation. --test-installation opens an
explicit disposable user installation, for real signed-repository tests only.
Each request is a single bounded JSON document; stdout contains JSON events.
"""
import hashlib
import json
import re
import sys
import gi

gi.require_version("Flatpak", "1.0")
from gi.repository import Flatpak, Gio, GLib


def emit(event, **fields):
    print(json.dumps({"event": event, **fields}, sort_keys=True), flush=True)


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,254}", value):
        raise ValueError("invalid identifier")
    return value


def ref_string(value):
    if not isinstance(value, str) or len(value) > 512:
        raise ValueError("invalid Flatpak reference")
    parts = value.split("/")
    if len(parts) != 4 or parts[0] not in ("app", "runtime"):
        raise ValueError("expected full app/name/architecture/branch reference")
    for item in parts[1:]:
        identifier(item)
    return value


def trusted(installation, name):
    identifier(name)
    remote = installation.get_remote_by_name(name, None)
    if remote.get_disabled() or not remote.get_gpg_verify():
        raise ValueError("repository disabled or signature verification disabled")
    return remote


def policy(installation):
    # Include the full native config and trusted key material, not only the URL.
    root = installation.get_path().get_path()
    from pathlib import Path
    h = hashlib.sha256()
    for path in [Path(root) / "repo/config", *sorted((Path(root) / "repo").glob("*.trustedkeys.gpg"))]:
        h.update(path.name.encode())
        h.update(path.read_bytes())
    return h.hexdigest()


def state(installation):
    return sorted([r.format_ref(), r.get_commit(), r.get_origin()]
                  for r in installation.list_installed_refs(None))


def package(ref, installed, remote=None):
    return {"id": ref.format_ref(), "name": (ref.get_appdata_name() or ref.get_name()) if installed and hasattr(ref, "get_appdata_name") else ref.get_name(), "summary": (ref.get_appdata_summary() or "") if installed and hasattr(ref, "get_appdata_summary") else "",
            "version": ref.get_commit() or "", "repository": remote or ref.get_origin(),
            "backend": "flatpak-system", "installed": installed,
            "download_bytes": 0 if installed else ref.get_download_size(),
            "installed_bytes": ref.get_installed_size(), "trust": "signature-required"}


def read_appstream(path, details_for=None):
    """Stream untrusted XML with bounded expansion and bounded retained fields."""
    import gzip
    import xml.etree.ElementTree as ET

    class BoundedReader:
        def __init__(self, source):
            self.source, self.total, self.tail = source, 0, b""

        def read(self, size=-1):
            data = self.source.read(min(size, 65536) if size >= 0 else 65536)
            self.total += len(data)
            if self.total > 64 * 1048576:
                raise ValueError("AppStream metadata exceeds bound")
            markup = self.tail + data
            if b"<!DOCTYPE" in markup or b"<!ENTITY" in markup:
                raise ValueError("AppStream entity declarations are not accepted")
            self.tail = markup[-16:]
            return data

    result = {}
    count = 0
    with gzip.open(path, "rb") as source:
        events = ET.iterparse(BoundedReader(source), events=("start", "end"))
        _, root = next(events)
        if root.tag != "components":
            raise ValueError("Invalid AppStream document root")
        for event, component in events:
            if event != "end" or component.tag != "component":
                continue
            count += 1
            if count > 50000:
                raise ValueError("AppStream component count exceeds bound")
            appid = component.findtext("id", "").removesuffix(".desktop")
            if appid and len(appid) <= 255 and (details_for is None or appid == details_for):
                info = {
                    "name": component.findtext("name", appid)[:256],
                    "summary": component.findtext("summary", "")[:2048],
                    "categories": [(c.text or "")[:64] for c in component.findall("categories/category")[:64]],
                }
                if details_for is not None:
                    description = component.find("description")
                    info.update({
                        "description": "\n".join(description.itertext())[:65536] if description is not None else "",
                        "license": component.findtext("project_license", "")[:1024],
                        "homepage": next((u.text or "" for u in component.findall("url") if u.attrib.get("type") == "homepage"), "")[:2048],
                    })
                result[appid] = info
            # Drop completed XML subtrees; searches retain only small summaries.
            component.clear()
            root.clear()
    return result


def appstream(installation, remote, details_for=None):
    """Read libflatpak's native cache; never create a package database."""
    from pathlib import Path
    directory = Path(remote.get_appstream_dir(None).get_path())
    path = directory / "appstream.xml.gz"
    if not path.exists():
        installation.update_appstream_sync(remote.get_name(), None, None, None)
    return read_appstream(path, details_for) if path.exists() else {}

def transaction(installation, request, expected=None, expires_at=None, cancellable=None):
    action = request["action"]
    ref = ref_string(request["package"])
    remote = identifier(request["repository"])
    trusted(installation, remote)
    tx = Flatpak.Transaction.new_for_installation(installation, None)
    tx.set_no_interaction(True)
    tx.connect("add-new-remote", lambda *args: False)
    tx.connect("choose-remote-for-ref", lambda tx, for_ref, runtime_ref, remotes: list(remotes).index(remote) if remote in remotes else -1)
    if action == "install":
        tx.add_install(remote, ref, None)
    elif action == "remove":
        current = installation.get_installed_ref(Flatpak.Ref.parse(ref).get_kind(),
                    *ref.split("/")[1:], None)
        if current.get_origin() != remote:
            raise ValueError("installed origin differs from requested repository")
        tx.add_uninstall(ref)
    elif action == "update":
        tx.add_update(ref, None, None)
    else:
        raise ValueError("unsupported transaction action")
    resolved = None
    failures = []

    def ready(tx):
        nonlocal resolved
        changes = []
        for op in tx.get_operations():
            origin = op.get_remote()
            if origin:
                trusted(installation, origin)
            changes.append({"action": int(op.get_operation_type()), "package": op.get_ref(),
                "repository": origin or "", "commit": op.get_commit() or "",
                "download_bytes": op.get_download_size(), "installed_bytes": op.get_installed_size()})
        resolved = {"schema_version": 1, "backend": "flatpak-system", "request": request,
                    "changes": sorted(changes, key=lambda x: (x["package"], x["action"])),
                    "installed_state": state(installation), "policy_fingerprint": policy(installation)}
        if expected is None:
            return False  # libflatpak aborts before applying any operation
        import time
        if expires_at is not None and (not isinstance(expires_at, int) or time.time() >= expires_at):
            failures.append("plan expired during native revalidation")
            return False
        if resolved != expected:
            failures.append("stale plan: native resolution or trust policy changed")
            return False
        return True

    tx.connect("ready", ready)
    tx.connect("operation-error", lambda tx, op, error, details: (failures.append(str(error)) or False))

    def new_operation(tx, op, progress):
        progress.connect("changed", lambda p: emit("progress", package=op.get_ref(),
                         percent=p.get_progress(), detail=p.get_status()))
    tx.connect("new-operation", new_operation)
    try:
        tx.run(cancellable)
    except GLib.Error as error:
        if failures:
            raise ValueError("; ".join(failures)) from error
        if expected is not None or resolved is None or not error.matches(Flatpak.error_quark(), Flatpak.Error.ABORTED):
            raise
    if failures:
        raise ValueError("; ".join(failures))
    if resolved is None:
        raise ValueError("native engine did not produce a plan")
    return resolved


def handle(installation, request, cancellable=None):
    mode = request["mode"]
    if mode == "repositories":
        from urllib.parse import urlsplit, urlunsplit
        def public_url(remote):
            url = urlsplit(remote.get_url())
            return urlunsplit((url.scheme, url.netloc.rsplit("@", 1)[-1], url.path, "", ""))
        return [{"id": r.get_name(), "url": public_url(r), "enabled": not r.get_disabled(),
                 "signature_required": r.get_gpg_verify()} for r in installation.list_remotes(None)]
    if mode == "installed":
        return [package(r, True) for r in installation.list_installed_refs(None)
                if r.get_kind() == Flatpak.RefKind.APP]
    if mode == "updates":
        result = []
        for ref in installation.list_installed_refs_for_update(None):
            if ref.get_kind() != Flatpak.RefKind.APP: continue
            origin = ref.get_origin()
            trusted(installation, origin)
            candidate = installation.fetch_remote_ref_sync(origin, ref.get_kind(), ref.get_name(), ref.get_arch(), ref.get_branch(), None)
            item = package(candidate, True, origin)
            item["summary"] = "Installed commit: " + ref.get_commit()
            result.append(item)
        return result
    if mode == "search":
        query = request.get("query", "").lower()
        category = request.get("category", "")
        if category: identifier(category)
        offset, limit = request.get("offset", 0), request.get("limit", 50)
        if not isinstance(query, str) or len(query) > 128 or not 0 <= offset <= 100000 or not 1 <= limit <= 100:
            raise ValueError("invalid search bounds")
        installed = {r.format_ref() for r in installation.list_installed_refs(None)}
        result = []
        for remote in installation.list_remotes(None):
            if remote.get_disabled() or not remote.get_gpg_verify():
                continue
            metadata = appstream(installation, remote)
            for ref in installation.list_remote_refs_sync(remote.get_name(), None):
                info = metadata.get(ref.get_name(), {})
                if ref.get_kind() == Flatpak.RefKind.APP and ref.get_arch() == Flatpak.get_default_arch() and query in (ref.get_name() + " " + info.get("name", "") + " " + info.get("summary", "")).lower() and (not category or category in info.get("categories", [])):
                    item = package(ref, ref.format_ref() in installed, remote.get_name())
                    item["name"] = info.get("name", item["name"])
                    item["summary"] = info.get("summary", "")
                    result.append(item)
        return sorted(result, key=lambda x: (x["name"], x["repository"], x["id"]))[offset:offset+limit]
    if mode == "details":
        ref = Flatpak.Ref.parse(ref_string(request["package"]))
        remote = identifier(request["repository"])
        trusted(installation, remote)
        remote_ref = installation.fetch_remote_ref_sync(remote, ref.get_kind(), ref.get_name(), ref.get_arch(), ref.get_branch(), None)
        result = package(remote_ref, any(r.format_ref() == ref.format_ref() for r in installation.list_installed_refs(None)), remote)
        metadata = installation.fetch_remote_metadata_sync(remote, remote_ref, None).get_data()
        if len(metadata) > 1048576:
            raise ValueError("package metadata exceeds bound")
        result["metadata"] = metadata.decode("utf-8")
        import configparser
        parsed = configparser.ConfigParser(interpolation=None, strict=True)
        parsed.read_string(result["metadata"])
        result["dependencies"] = parsed.get("Application", "runtime", fallback="Not provided")
        result.update(appstream(installation, trusted(installation, remote), ref.get_name()).get(ref.get_name(), {}))
        return result
    if mode in ("plan", "commit"):
        if set(request["request"]) != {"action", "package", "repository"}:
            raise ValueError("unexpected transaction fields")
        return transaction(installation, request["request"], request.get("expected") if mode == "commit" else None, request.get("expires_at"), cancellable)
    raise ValueError("unsupported bridge operation")


def main():
    if sys.argv[1:]:
        if len(sys.argv) != 3 or sys.argv[1] != "--test-installation":
            raise ValueError("invalid arguments")
        installation = Flatpak.Installation.new_for_path(Gio.File.new_for_path(sys.argv[2]), True, None)
    else:
        installation = Flatpak.Installation.new_system(None)
    raw = sys.stdin.buffer.readline(1048577)
    if len(raw) > 1048576:
        raise ValueError("request exceeds bound")
    import threading
    cancellable = Gio.Cancellable.new()
    def controls():
        if sys.stdin.buffer.readline(16).strip() == b"cancel":
            cancellable.cancel()
    threading.Thread(target=controls, daemon=True).start()
    emit("result", value=handle(installation, json.loads(raw), cancellable))

if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit("error", code="backend_failure", detail=str(error))
        sys.exit(1)
