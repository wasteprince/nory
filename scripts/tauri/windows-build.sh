#!/usr/bin/env bash
# LLVM tools (clang-cl, llvm-lib, llvm-rc) and cargo-xwin must be on PATH.
# xwin downloads the Microsoft SDK into the dedicated build cache.
set -euo pipefail
project="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
windows_build="${1:?dedicated Windows cache}"
[[ "$windows_build" = /* && "$windows_build" != / ]] || exit 2
mkdir -p "$windows_build"
export XWIN_CACHE_DIR="$windows_build/xwin"
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld"
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS='-C target-feature=+crt-static'
cargo-xwin build --release --locked --manifest-path "$project/desktop/src-tauri/Cargo.toml" --target x86_64-pc-windows-msvc
cargo-xwin build --release --locked --manifest-path "$project/Cargo.toml" --no-default-features --bin nory-helper --target x86_64-pc-windows-msvc
