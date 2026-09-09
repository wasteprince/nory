#!/usr/bin/env bash
# Repackage already-built release binaries into fresh staging directories.
# Usage: bash scripts/tauri/package.sh /absolute/output-directory /absolute/ubuntu-build /absolute/windows-build
set -euo pipefail
project="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
release_version=$(sed -n 's/^version = "\([0-9.]*\)"/\1/p' "$project/desktop/src-tauri/Cargo.toml" | head -n 1)
[[ "$release_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 2
output="${1:?new output directory}"; ubuntu="${2:?Ubuntu build cache}"; windows="${3:?Windows build cache}"
[[ "$output" = /* && ! -e "$output" && -x "$ubuntu/rootfs/usr/bin/dpkg-deb" && -x "$ubuntu/rootfs/usr/bin/makensis" ]] || exit 2
[[ "$ubuntu" = /* && "$windows" = /* ]] || exit 2
mkdir -p "$output/arch" "$output/debian" "$output/windows"
bash "$project/scripts/tauri/stage-linux.sh" "$output/arch/stage" "$project/desktop/src-tauri/target/release/nory" "$project/target/release/nory-helper"
install -m644 "$project/packaging/tauri/arch/PKGBUILD" "$project/packaging/nory.install" "$output/arch/"
(cd "$output/arch" && makepkg --nodeps --nocheck)
bash "$project/scripts/tauri/stage-linux.sh" "$output/debian/stage" "$ubuntu/target/release/nory" "$ubuntu/helper-target/release/nory-helper"
install -Dm644 "$project/packaging/tauri/debian/control" "$output/debian/stage/DEBIAN/control"
install -m755 "$project/packaging/debian/postinst" "$project/packaging/debian/prerm" "$project/packaging/debian/postrm" "$output/debian/stage/DEBIAN/"
bwrap --unshare-user --uid 0 --gid 0 --bind "$ubuntu/rootfs" / --proc /proc --dev /dev --bind "$output/debian" /package \
  --setenv PATH /usr/bin:/bin /usr/bin/dpkg-deb --root-owner-group -Zxz --build /package/stage "/package/nory_${release_version}_amd64.deb"
python3 "$project/scripts/tauri/stage-windows.py" --build-root "$windows" --stage "$output/windows/stage"
# Pin the exact Microsoft bootstrapper verified for this build. A later version
# requires an explicit hash update, not silent trust in an unverified download.
bootstrap="$windows/MicrosoftEdgeWebview2Setup.exe"
if [[ ! -e "$bootstrap" ]]; then
  curl --fail --location --proto '=https' --proto-redir '=https' 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -o "$bootstrap"
fi
[[ "$(sha256sum "$bootstrap" | cut -d' ' -f1)" = 17debf797a6c737959bc588236e897936ffac1af5f7e515e674ab32f9edfe719 ]] || { echo 'WebView2 bootstrapper checksum mismatch' >&2; exit 1; }
bwrap --unshare-user --uid 0 --gid 0 --bind "$ubuntu/rootfs" / --proc /proc --dev /dev \
  --ro-bind "$windows" /windows --ro-bind "$project" /project --bind "$output/windows" /output --setenv PATH /usr/bin:/bin \
  /usr/bin/makensis -INPUTCHARSET UTF8 -V3 -DTAURI_UI "-DVERSION=$release_version" -DSTAGE=/output/stage \
  "-DOUTPUT=/output/NORY-$release_version-windows-x64-setup.exe" -DICON_FILE=/project/assets/icons/io.nory.NORY.ico \
  -DPLUGIN_DIR=/windows/nsis-plugin/Plugins/x86-unicode -DUNINSTALL_INCLUDE=/output/uninstall-files.nsh \
  -DWEBVIEW_BOOTSTRAPPER=/windows/MicrosoftEdgeWebview2Setup.exe /project/packaging/windows/nory.nsi
sha256sum "$output/arch/nory-$release_version-1-x86_64.pkg.tar.zst" "$output/debian/nory_${release_version}_amd64.deb" "$output/windows/NORY-$release_version-windows-x64-setup.exe"
