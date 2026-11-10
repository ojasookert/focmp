#!/usr/bin/env bash
# Build version.dll (loader, C/mingw) and focmp.dll (Rust) for Windows x64.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$root/dist"

x86_64-w64-mingw32-gcc -O2 -Wall -shared -s \
    -o "$root/dist/version.dll" "$root/loader/version.c" "$root/loader/version.def"

cargo build --locked --release --target x86_64-pc-windows-gnu --manifest-path "$root/focmp/Cargo.toml"
cp "$root/focmp/target/x86_64-pc-windows-gnu/release/focmp.dll" "$root/dist/focmp.dll"

cp "$root/focmp.ini" "$root/dist/focmp.ini"

ls -la "$root/dist"
