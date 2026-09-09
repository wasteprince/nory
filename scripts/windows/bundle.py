#!/usr/bin/env python3
"""Stage a self-contained Windows 11 x64 installation in a new directory."""
import argparse
import concurrent.futures
import hashlib
import io
import json
from pathlib import Path
import re
import shutil
import subprocess
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = {
    "xray": ("https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-windows-64.zip",
             "d004c39288ce9ada487c6f398c7c545f7d749e44bdfdd59dbc9f865afba4e1ad"),
    "sing-box": ("https://github.com/SagerNet/sing-box/releases/download/v1.13.18/sing-box-1.13.18-windows-amd64.zip",
                 "65045155ffdc506334f01a4353889657ddfc024f72b394081a9abaef34dfbef3"),
    "mihomo": ("https://github.com/MetaCubeX/mihomo/releases/download/v1.19.30/mihomo-windows-amd64-compatible-v1.19.30.zip",
               "289fde5e29d37a5b3326480590d8b3551c5bf7f8737290355c19bce74d57a563"),
    "wintun": ("https://www.wintun.net/builds/wintun-0.14.1.zip",
               "07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51"),
    "shell-plugin": ("https://nsis.sourceforge.io/mediawiki/images/6/68/ShellExecAsUser_amd64-Unicode.7z",
                     "0a55ea25c7330a92cec028eda8afcaf1b1a7092e0dfb77c21c8f654564b4ff9d"),
}
# These are Windows system components/API sets, not redistributable SDK DLLs.
SYSTEM_DLLS = set("advapi32 bcrypt cabinet cfgmgr32 comctl32 comdlg32 crypt32 cryptbase cryptnet cryptsp d2d1 d3d11 d3d12 dcomp debug dbghelp dnsapi dwmapi dwrite dxgi dxva2 gdi32 hid imm32 iphlpapi kernel32 kernelbase mpr msimg32 msvcrt ncrypt netapi32 ntdll ole32 oleacc oleaut32 opengl32 powrprof propsys psapi rpcrt4 secur32 setupapi shell32 shlwapi synchronization ucrtbase urlmon user32 userenv usp10 uxtheme version win32u winhttp wininet winmm winspool wintrust wldap32 ws2_32 wtsapi32 avrt fwpuclnt normaliz mfuuid mf mfplat mfreadwrite ksuser msdmo msasn1 nsi wsock32".split())
SYSTEM_DLLS.update(["bcryptprimitives", "gdiplus", "shcore", "combase", "winspool.drv"])


def fetch(name, downloads):
    url, digest = ARTIFACTS[name]
    destination = downloads / url.rsplit("/", 1)[1]
    if not destination.exists():
        request = urllib.request.Request(url, headers={"User-Agent": "NORY-build/1"})
        with urllib.request.urlopen(request, timeout=180) as response:
            contents = response.read()
        if hashlib.sha256(contents).hexdigest() != digest:
            raise RuntimeError(f"Checksum mismatch for {name}")
        with destination.open("xb") as output:
            output.write(contents)
    with destination.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != digest:
            raise RuntimeError(f"Checksum mismatch for cached {name}")
    return name, destination


def imports(path, objdump):
    output = subprocess.check_output([objdump, "-p", str(path)], text=True)
    return re.findall(r"DLL Name:\s*(\S+)", output)


def copy_dependencies(stage, sdk, objdump):
    available = {path.name.lower(): path for path in (sdk / "bin").glob("*.dll")}
    queue = list(stage.rglob("*.exe")) + list(stage.rglob("*.dll"))
    seen = set()
    while queue:
        binary = queue.pop()
        if binary in seen:
            continue
        seen.add(binary)
        for name in imports(binary, objdump):
            key = name.lower()
            if key.startswith(("api-ms-", "ext-ms-")) or key.removesuffix(".dll") in SYSTEM_DLLS:
                continue
            # Core DLLs (Wintun/Cronet) belong next to their core executable.
            if (binary.parent / name).is_file() or (stage / name).is_file():
                continue
            if key not in available:
                raise RuntimeError(f"Unresolved DLL {name} required by {binary.name}")
            destination = stage / available[key].name
            shutil.copy2(available[key], destination)
            queue.append(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--build-root", type=Path, required=True)
    parser.add_argument("--stage", type=Path, required=True)
    parser.add_argument("--objdump", default="x86_64-w64-mingw32-objdump")
    args = parser.parse_args()
    stage, sdk = args.stage.resolve(), args.sdk.resolve()
    if stage.exists():
        raise RuntimeError("Stage must be a new directory; existing installations are never overwritten")
    stage.mkdir(parents=True)
    downloads = args.build_root / "downloads"
    downloads.mkdir(parents=True, exist_ok=True)
    for binary in ["nory.exe", "nory-helper.exe"]:
        shutil.copy2(ROOT / "target/x86_64-pc-windows-gnu/release" / binary, stage / binary)
    licenses = stage / "licenses"
    licenses.mkdir()
    shutil.copy2(ROOT / "LICENSE", licenses / "NORY-LICENSE")
    # Include exact build lock and provenance, but never user configs/HWIDs.
    shutil.copy2(ROOT / "scripts/windows/msys2-runtime.lock.json", licenses / "msys2-runtime.lock.json")
    (licenses / "cores.lock.json").write_text(json.dumps(ARTIFACTS, indent=2) + "\n")
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
        artifacts = dict(executor.map(lambda name: fetch(name, downloads), ARTIFACTS))
    for core in ["xray", "sing-box", "mihomo"]:
        destination = stage / "cores" / core
        destination.mkdir(parents=True)
        with zipfile.ZipFile(artifacts[core]) as archive:
            for name in archive.namelist():
                base = Path(name).name
                if base.lower().endswith(".exe"):
                    if core == "xray" and base != "xray.exe":
                        continue
                    with (destination / f"{core}.exe").open("xb") as output:
                        output.write(archive.read(name))
                elif base.lower().endswith(".dll") or base in ["geoip.dat", "geosite.dat"]:
                    with (destination / base).open("xb") as output:
                        output.write(archive.read(name))
                elif base.upper().startswith(("LICENSE", "COPYING")):
                    (licenses / f"{core}-{base}").write_bytes(archive.read(name))
        if not (destination / f"{core}.exe").is_file():
            raise RuntimeError(f"Missing {core}.exe in official archive")
    with zipfile.ZipFile(artifacts["wintun"]) as archive:
        dll = archive.read("wintun/bin/amd64/wintun.dll")
        for core in ["sing-box", "mihomo"]:
            (stage / "cores" / core / "wintun.dll").write_bytes(dll)
        (licenses / "Wintun-LICENSE.txt").write_bytes(archive.read("wintun/LICENSE.txt"))
    plugin = args.build_root / "nsis-plugin"
    plugin.mkdir(exist_ok=True)
    subprocess.run(["bsdtar", "-xf", str(artifacts["shell-plugin"]), "-C", str(plugin)], check=True)
    shutil.copytree(plugin / "Docs/ShellExecAsUser", licenses / "ShellExecAsUser")
    shutil.copy2(plugin / "Contrib/ShellExecAsUser/VistaTools.cxx", licenses / "ShellExecAsUser/VistaTools.cxx")
    # gdk-pixbuf SVG loader is dynamically loaded and invisible to PE imports.
    for relative in ["lib/gdk-pixbuf-2.0/2.10.0/loaders", "share/glib-2.0/schemas",
                     "share/icons/Adwaita", "share/icons/AdwaitaLegacy", "share/icons/hicolor", "share/locale/ru"]:
        source = sdk / relative
        if source.exists():
            shutil.copytree(source, stage / relative, ignore=shutil.ignore_patterns("*.a", "*.pc"))
    for item in (sdk / "share/licenses").iterdir():
        if item.is_dir():
            shutil.copytree(item, licenses / "runtime" / item.name)
        else:
            destination = licenses / "runtime" / item.name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(item, destination)
    for name in ["gdk-pixbuf-query-loaders.exe", "glib-compile-schemas.exe"]:
        shutil.copy2(sdk / "bin" / name, stage / name)
    copy_dependencies(stage, sdk, args.objdump)
    icon_path = stage / "share/icons/hicolor/256x256/apps"
    icon_path.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "assets/icons/io.nory.NORY-256.png", icon_path / "io.nory.NORY.png")
    # A separate icon path also invalidates shortcuts which cached the old
    # nory.exe resource. It is generated from the same canonical SVG.
    shutil.copy2(ROOT / "assets/icons/io.nory.NORY.ico", stage / "nory.ico")
    settings = stage / "etc/gtk-4.0"
    settings.mkdir(parents=True)
    (settings / "settings.ini").write_text("[Settings]\ngtk-font-name=Segoe UI 10\ngtk-icon-theme-name=Adwaita\ngtk-application-prefer-dark-theme=true\n")
    # Store a build BOM and produce exact uninstall entries. No recursive
    # deletion of Program Files/NORY or user's unrelated files is necessary.
    files = sorted(path for path in stage.rglob("*") if path.is_file())
    (stage / "installed-files.json").write_text(json.dumps({str(path.relative_to(stage)): hashlib.file_digest(path.open("rb"), "sha256").hexdigest() for path in files}, indent=2) + "\n")
    files.append(stage / "installed-files.json")
    uninstall = [f'Delete /REBOOTOK "$INSTDIR\\{str(path.relative_to(stage)).replace(chr(47), chr(92))}"' for path in files]
    for path in sorted((p for p in stage.rglob("*") if p.is_dir()), key=lambda p: len(p.parts), reverse=True):
        uninstall.append(f'RMDir "$INSTDIR\\{str(path.relative_to(stage)).replace(chr(47), chr(92))}"')
    (stage.parent / "uninstall-files.nsh").write_text("\n".join(uninstall) + "\n")
    print(f"Staged {len(files)} files, flags embedded in nory.exe: {stage}")


if __name__ == "__main__":
    main()
