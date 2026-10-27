#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
if ! command -v cargo >/dev/null 2>&1; then
    echo 'SKIP: cargo/Rust toolchain is not installed' >&2
    exit 77
fi
cargo build
cargo run --quiet -- bootstrap-selfhost
binary=./selfhost/target/nova-selfhost
for file in selfhost/tests/valid_*.nova; do
    NOVA_SOURCE="$file" "$binary" || exit 1
done
for file in selfhost/tests/invalid_*.nova; do
    if NOVA_SOURCE="$file" "$binary"; then
        echo "Unexpectedly accepted invalid source: $file" >&2
        exit 1
    fi
done
for file in examples/*.nova; do
    case "$file" in *invalid.nova) continue ;; esac
    NOVA_SOURCE="$file" "$binary" || exit 1
done
# Native and self-written parsers must both accept baseline examples.
for file in examples/hello.nova examples/arith.nova examples/structs.nova; do
    cargo run --quiet -- parse-selfhost "$file" || exit 1
    NOVA_SELFHOST_PARSER="$PWD/$binary" cargo run --quiet -- check "$file" || exit 1
done
# Compare syntax accept/reject outcomes and validate exported CST-V1 trees.
python3 selfhost/tools/differential.py target/debug/nova "$binary" --seed 94831 --count 100
echo 'Selfhost parser smoke tests: OK' 
