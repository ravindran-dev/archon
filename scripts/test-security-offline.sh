#!/usr/bin/env bash
# Run the actual locked workspace modules, without a separate dependency graph.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --locked --offline -p archon-cli archive::tests "$@"
cargo test --locked --offline -p archon-cli handoff::tests "$@"
cargo test --locked --offline -p archon-cli pgp::tests "$@"
cargo test --locked --offline -p archon-sandbox "$@"
