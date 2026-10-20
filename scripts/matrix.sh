#!/usr/bin/env bash
# CLI smoke matrix; --install also tests native jq install/remove in containers.
# All package bootstrap and transactions happen only in disposable images.
# No privileged containers; sandbox/eBPF checks are intentionally separate.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p audit/evidence/matrix
images=(archlinux:latest ubuntu:24.04 debian:stable fedora:latest opensuse/tumbleweed alpine:latest)
failed=0
mode=smoke
if [[ $# -eq 1 && "${1:-}" == --install ]]; then mode=install
elif [[ $# -ne 0 ]]; then echo 'Usage: scripts/matrix.sh [--install]' >&2; exit 2; fi
printf 'IMAGE\tMODE\tRESULT\tREASON\n'
reason=''
if ! command -v docker >/dev/null 2>&1; then reason='docker not installed'
elif ! docker info >audit/evidence/matrix/docker-info.log 2>&1; then reason='docker daemon unavailable (see docker-info.log)'; fi
if [[ -n "$reason" ]]; then
    for image in "${images[@]}"; do
        printf '%s\t%s\tUNVERIFIED\t%s\n' "$image" "$mode" "$reason"
    done
    exit 2
fi
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
# Static musl executable runs on both Alpine and glibc images. Build from
# tracked/worktree sources, excluding local artifacts and credentials.
tar --exclude=.git --exclude=target --exclude=audit/evidence -cf - . | tar -xf - -C "$context"
for image in "${images[@]}"; do
    key=${image//[^a-zA-Z0-9]/-}
    log="audit/evidence/matrix/$key-$mode.log"
    runtime=()
    if [[ "$mode" == smoke ]]; then runtime=(--network=none --read-only --tmpfs /tmp); fi
    if docker build --build-arg "DISTRO=$image" -f "$context/scripts/matrix.Dockerfile" -t "archon-audit:$key" "$context" >"$log" 2>&1 &&
       docker run --rm "${runtime[@]}" "archon-audit:$key" "$mode" >>"$log" 2>&1; then
        printf '%s\t%s\tPASS\tsee %s\n' "$image" "$mode" "$log"
    else
        printf '%s\t%s\tFAIL\tsee %s\n' "$image" "$mode" "$log"
        failed=1
    fi
done
exit "$failed"
