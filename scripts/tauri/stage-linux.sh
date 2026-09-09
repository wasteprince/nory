#!/usr/bin/env bash
# Package only explicit application assets; never copy user state or a source tree.
set -euo pipefail
project="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
stage="${1:?new absolute staging directory}"; gui="${2:?GUI executable}"; helper="${3:?helper executable}"
[[ "$stage" = /* && ! -e "$stage" && -x "$gui" && -x "$helper" ]] || exit 2
install -Dm755 "$gui" "$stage/usr/bin/nory"
install -Dm755 "$helper" "$stage/usr/lib/nory/nory-helper"
for core in xray sing-box mihomo; do
  install -Dm755 "$project/.bundled/arch/$core/$core" "$stage/usr/lib/nory/cores/$core/$core"
  install -Dm644 "$project/.bundled/arch/$core/LICENSE" "$stage/usr/share/licenses/nory/$core-LICENSE"
done
for data in geoip.dat geosite.dat; do
  install -Dm644 "$project/.bundled/arch/xray/$data" "$stage/usr/lib/nory/cores/xray/$data"
done
install -Dm755 "$project/.bundled/arch/sing-box/libcronet.so" "$stage/usr/lib/nory/cores/sing-box/libcronet.so"
for unit in nory-helper.socket nory-helper.service; do
  install -Dm644 "$project/packaging/$unit" "$stage/usr/lib/systemd/system/$unit"
done
install -Dm644 "$project/packaging/io.nory.NORY.desktop" "$stage/usr/share/applications/io.nory.NORY.desktop"
install -Dm644 "$project/assets/io.nory.NORY.svg" "$stage/usr/share/icons/hicolor/scalable/apps/io.nory.NORY.svg"
for size in 16 32 48 128 256; do
  install -Dm644 "$project/assets/icons/io.nory.NORY-$size.png" "$stage/usr/share/icons/hicolor/${size}x${size}/apps/io.nory.NORY.png"
done
install -Dm644 "$project/assets/icons/io.nory.NORY-128.png" "$stage/usr/share/pixmaps/io.nory.NORY.png"
install -Dm644 "$project/LICENSE" "$stage/usr/share/licenses/nory/LICENSE"
install -Dm644 "$project/desktop/README.md" "$stage/usr/share/doc/nory/README.md"
install -Dm644 "$project/assets/maps/README.md" "$stage/usr/share/licenses/nory/world-map.md"
for dependency in vue @vue/shared @vue/reactivity @vue/runtime-core @vue/runtime-dom @lucide/vue @tauri-apps/api @tauri-apps/plugin-dialog tailwindcss; do
  for license in "$project/desktop/node_modules/$dependency"/LICENSE*; do
    [[ -f "$license" ]] || continue
    name="${dependency//\//-}"
    install -Dm644 "$license" "$stage/usr/share/licenses/nory/frontend/$name-$(basename "$license")"
  done
done
