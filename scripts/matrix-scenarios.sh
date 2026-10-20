#!/bin/sh
set -eu
# Same scenarios in each real distro. No installs, removals, network, or root
# capabilities needed. The Arch image bootstraps sync DBs during image build.
unset ARCHON_OS_RELEASE
archon --version
archon --help
archon install --help
archon remove --help
archon build --help
case "$(. /etc/os-release; printf '%s' "$ID")" in
    arch) expected='pacman' ;;
    ubuntu|debian) expected='apt' ;;
    fedora) expected='dnf' ;;
    opensuse*) expected='zypper' ;;
    alpine) expected='apk' ;;
    *) echo 'FAIL: unexpected distro'; exit 1 ;;
esac
archon install bash --dry-run > /tmp/plan
cat /tmp/plan
if [ "$expected" = pacman ]; then
    grep -F 'Install plan' /tmp/plan
else
    grep -F "delegating to $expected" /tmp/plan
fi
# Fixture tests never consult the native package database.
printf 'ID=ubuntu\nID_LIKE=arch\n' > /tmp/os-release
ARCHON_OS_RELEASE=/tmp/os-release archon install bash --dry-run > /tmp/fixture-plan
cat /tmp/fixture-plan
grep -F 'delegating to apt' /tmp/fixture-plan
printf 'ID=unknown-test\n' > /tmp/os-release
if ARCHON_OS_RELEASE=/tmp/os-release archon plan bash > /tmp/unknown 2>&1; then
    echo 'FAIL: unsupported distro accepted'; exit 1
fi
cat /tmp/unknown
grep -F 'no supported package manager' /tmp/unknown

if [ "${1:-smoke}" = install ]; then
    # Real native transactions inside this disposable container, as a normal
    # user with container-local sudo. No host mounts or privileged mode.
    printf 'y\n' | archon install jq --yes
    jq --version
    printf 'y\n' | archon remove jq --yes
    if command -v jq >/dev/null 2>&1; then
        echo 'FAIL: jq is still installed after removal'; exit 1
    fi
    echo 'PASS: native jq installation and removal'
fi

if [ "$expected" = apt ]; then
    archon-verify-dpkg
fi
