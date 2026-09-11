#!/bin/sh
set -eu

command -v readelf strings gzip >/dev/null

ifstack_separator=$(printf '\037')
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+$CARGO_ENCODED_RUSTFLAGS$ifstack_separator}--remap-path-prefix=$HOME=/build$ifstack_separator--remap-path-prefix=$PWD=/source$ifstack_separator--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
cargo build --locked --release --target x86_64-unknown-linux-musl
binary=target/x86_64-unknown-linux-musl/release/agentx-ifstack
if readelf -l "$binary" | grep -q INTERP; then
    echo 'The package binary must be static.' >&2
    exit 1
fi
if readelf -d "$binary" | grep -q NEEDED; then
    echo 'The package binary must not depend on shared libraries.' >&2
    exit 1
fi
if readelf -S "$binary" | grep -q '\.symtab'; then
    echo 'The package binary must be stripped.' >&2
    exit 1
fi
if strings "$binary" | grep -F -e "$HOME/" -e "$PWD/" -e "${CARGO_HOME:-$HOME/.cargo}/"; then
    echo 'The package binary contains a build path.' >&2
    exit 1
fi
mkdir -p target/man dist
gzip -n -9 -c packaging/agentx-ifstack.8 > target/man/agentx-ifstack.8.gz
cargo deb --no-build --no-strip --target x86_64-unknown-linux-musl --output dist/
cargo generate-rpm --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/generate-rpm/*.rpm dist/
