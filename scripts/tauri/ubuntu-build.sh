#!/usr/bin/env bash
# An isolated Ubuntu 24.04 build, not a repackaged Arch executable.
set -euo pipefail
project="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
build_root="${1:?dedicated absolute build directory required}"
[[ "$build_root" = /* && "$build_root" != / ]] || exit 2
mkdir -p "$build_root"
if [[ ! -f "$build_root/rootfs/.nory-prepared" ]]; then
  python3 "$project/scripts/tauri/ubuntu-root.py" "$build_root"
  unshare --user --map-auto --map-root-user --setgroups allow bwrap --unshare-pid --unshare-ipc --unshare-uts --uid 0 --gid 0 --bind "$build_root/rootfs" / \
    --proc /proc --dev /dev --ro-bind /etc/resolv.conf /etc/resolv.conf \
    --setenv DEBIAN_FRONTEND noninteractive --setenv PATH /usr/sbin:/usr/bin:/sbin:/bin \
    /bin/sh -c 'dpkg --configure -a && apt-get -o APT::Sandbox::User=root update && apt-get -o APT::Sandbox::User=root install -y --no-install-recommends build-essential pkg-config libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev ca-certificates patchelf nsis && touch /.nory-prepared'
fi
mkdir -p "$build_root/cargo" "$build_root/target" "$build_root/helper-target" "$build_root/generated"
rust_toolchain="$(rustc --print sysroot)"
unshare --user --map-auto --map-root-user --setgroups allow bwrap --unshare-pid --unshare-ipc --unshare-uts --uid 0 --gid 0 --bind "$build_root/rootfs" / \
  --proc /proc --dev /dev --ro-bind /etc/resolv.conf /etc/resolv.conf \
  --ro-bind "$rust_toolchain" /opt/rust --ro-bind "$project" /project \
  --bind "$build_root/generated" /project/desktop/src-tauri/gen \
  --bind "$build_root/cargo" /cargo --bind "$build_root/target" /target \
  --bind "$build_root/helper-target" /helper-target \
  --setenv PATH /opt/rust/bin:/usr/bin:/bin --setenv CARGO_HOME /cargo \
  --setenv CARGO_TARGET_DIR /target --chdir /project/desktop/src-tauri \
  /bin/sh -c 'cargo build --release --locked --features debian-package && cd /project && CARGO_TARGET_DIR=/helper-target cargo build --release --locked --no-default-features --features debian-package --bin nory-helper'
