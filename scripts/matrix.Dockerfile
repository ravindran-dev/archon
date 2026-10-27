ARG DISTRO=archlinux:latest
FROM rust:alpine AS build
RUN apk add --no-cache build-base musl-dev cmake perl linux-headers
WORKDIR /src
COPY . .
RUN cargo build --locked --release --workspace

FROM ${DISTRO}
USER root
# Bootstrap only inside disposable images; never run these on the host.
RUN set -eu; \
    if command -v pacman >/dev/null; then \
        pacman -Syu --noconfirm --needed base-devel git bubblewrap sudo; \
    elif command -v apt-get >/dev/null; then \
        apt-get update && apt-get install -y sudo passwd ca-certificates; \
    elif command -v dnf >/dev/null; then \
        dnf install -y sudo shadow-utils ca-certificates python3-libdnf5; \
    elif command -v zypper >/dev/null; then \
        zypper --non-interactive refresh && zypper --non-interactive install sudo shadow ca-certificates; \
    elif command -v apk >/dev/null; then \
        apk add --no-cache sudo shadow ca-certificates; \
    else exit 1; fi; \
    useradd -m -s /bin/sh archon; \
    mkdir -p /etc/sudoers.d; \
    echo 'archon ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/archon; \
    chmod 0440 /etc/sudoers.d/archon
COPY --from=build /src/target/release/archon /usr/local/bin/archon
COPY --from=build /src/target/release/verify_dpkg /usr/local/bin/archon-verify-dpkg
COPY scripts/matrix-scenarios.sh /scenarios.sh
USER archon
ENV HOME=/home/archon
ENTRYPOINT ["sh", "/scenarios.sh"]
