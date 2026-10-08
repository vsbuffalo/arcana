#!/bin/sh
# Install arcana and set it up.
#
#   curl -fsSL https://raw.githubusercontent.com/vsbuffalo/arcana/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --public-host notes.example.com
#
# Builds arcana with cargo, then runs `arcana setup`, which creates the vault,
# installs the background server and connects Claude Code. Arguments are passed
# to `arcana setup` (see `arcana setup --help`). Safe to re-run.
set -eu

REPO="https://github.com/vsbuffalo/arcana"

if ! command -v cargo >/dev/null 2>&1; then
    echo "arcana needs the Rust toolchain to build (prebuilt binaries are not published yet)." >&2
    echo "Install it from https://rustup.rs, then run this again." >&2
    exit 1
fi

echo "Building arcana (first build takes a few minutes)..." >&2
cargo install --locked --git "$REPO" arcana-cli

exec "${CARGO_HOME:-$HOME/.cargo}/bin/arcana" setup "$@"
