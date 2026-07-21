#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

printf '==> Formatting\n'
cargo fmt

printf '==> Running tests\n'
cargo test

printf '==> Building release binary\n'
cargo build --release

printf '==> Installing wtfis and cdd\n'
cargo install --path . --force

printf '\nUpdated successfully. Try: wtfis\n'
