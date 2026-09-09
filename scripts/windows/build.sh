#!/usr/bin/env bash
set -euo pipefail

# Usage: bash scripts/windows/build.sh /absolute/build-directory
# Needs Rust's x86_64-pc-windows-gnu target, matching MinGW GCC, bsdtar,
# pkg-config, Python 3 and Wine on PATH. No system packages are installed.
nory_build_root="${1:?pass a dedicated absolute build directory}"
[[ "$nory_build_root" = /* && "$nory_build_root" != / ]] || exit 2
nory_project="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$nory_project"
bash scripts/render-icons.sh
mkdir -p -- "$nory_build_root"
python3 scripts/windows/fetch-runtime.py --root "$nory_build_root" --lock scripts/windows/msys2-runtime.lock.json --locked
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$nory_build_root/sdk"
export PKG_CONFIG_LIBDIR="$nory_build_root/sdk/mingw64/lib/pkgconfig:$nory_build_root/sdk/mingw64/share/pkgconfig"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export WINEPREFIX="$nory_build_root/wine"
export WINEDEBUG=-all
export WINEDLLOVERRIDES='mscoree,mshtml='
nory_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
cargo build --release --locked --target x86_64-pc-windows-gnu --bins
nory_stage_parent="$(mktemp -d "$nory_build_root/staging-XXXXXX")"
python3 scripts/windows/bundle.py --sdk "$nory_build_root/sdk/mingw64" --build-root "$nory_build_root" --stage "$nory_stage_parent/payload"
nory_output="$nory_stage_parent/NORY-$nory_version-windows-x64-setup.exe"
wine "$nory_build_root/sdk/mingw64/bin/makensis.exe" /INPUTCHARSET UTF8 /V3 \
  "/DVERSION=$nory_version" \
  "/DSTAGE=$(winepath -w "$nory_stage_parent/payload")" \
  "/DOUTPUT=$(winepath -w "$nory_output")" \
  "/DICON_FILE=$(winepath -w "$nory_project/assets/icons/io.nory.NORY.ico")" \
  "/DPLUGIN_DIR=$(winepath -w "$nory_build_root/nsis-plugin/Plugins/amd64-unicode")" \
  "/DUNINSTALL_INCLUDE=$(winepath -w "$nory_stage_parent/uninstall-files.nsh")" \
  "$(winepath -w "$nory_project/packaging/windows/nory.nsi")"
sha256sum "$nory_output"
printf 'Installer: %s\n' "$nory_output"
