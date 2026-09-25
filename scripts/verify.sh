#!/usr/bin/env bash
# The macOS verification matrix: formatting, lints for the host and for
# x86_64-pc-windows-msvc (type-check only, no linker needed), then native tests.
set -euo pipefail

cd "$(dirname "$0")/.."

# Live TDLib tests run against the flox-mac vendored dylib when it is present.
TDJSON_MAC="/Users/ryantellissy/Desktop/flox-mac/Vendor/tdlib/lib/libtdjson.dylib"
if [[ -z "${FLOX_TDJSON:-}" && -f "$TDJSON_MAC" ]]; then
    export FLOX_TDJSON="$TDJSON_MAC"
fi

step() {
    printf '\n==> %s\n' "$*"
    "$@"
}

step cargo fmt --all --check
step cargo clippy --workspace --all-targets -- -D warnings
step cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
step cargo test --workspace

printf '\nverify: all checks passed\n'
