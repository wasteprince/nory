#!/usr/bin/env python3
"""Fetch a pinned, checksum-verified MSYS2 MINGW64 SDK without installing it.

The GNU Windows Rust target and this SDK both use MSVCRT. Do not mix these
libraries with UCRT64 or MSYS /usr DLLs. The resulting lock is a build input;
pass --locked on subsequent builds to reproduce the exact dependency set.
"""
import argparse
import concurrent.futures
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import urllib.request

BASE = "https://repo.msys2.org/mingw/mingw64/"
ROOTS = ["mingw-w64-x86_64-" + name for name in (
    "gtk4", "libadwaita", "adwaita-icon-theme", "hicolor-icon-theme", "nsis")]


def download(url):
    request = urllib.request.Request(url, headers={"User-Agent": "NORY-build/1"})
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def resolve():
    database = download(BASE + "mingw64.db")
    entries = {}
    providers = {}
    with tarfile.open(fileobj=io.BytesIO(database), mode="r:*") as archive:
        for member in archive.getmembers():
            if not member.name.endswith("/desc"):
                continue
            fields = {}
            for field in archive.extractfile(member).read().decode().split("\n\n"):
                lines = field.strip().splitlines()
                if lines:
                    fields[lines[0].strip("%")] = lines[1:]
            name = fields["NAME"][0]
            entries[name] = fields
            for provided in fields.get("PROVIDES", []):
                providers[re.split(r"[<>=]", provided)[0]] = name
    queue, selected = list(ROOTS), {}
    while queue:
        name = re.split(r"[<>=]", queue.pop())[0]
        name = name if name in entries else providers.get(name, name)
        if name in selected:
            continue
        if name not in entries:
            raise RuntimeError(f"Unresolved runtime dependency: {name}")
        item = entries[name]
        selected[name] = {key.lower(): item[key][0] for key in ("NAME", "VERSION", "FILENAME", "SHA256SUM")}
        queue.extend(item.get("DEPENDS", []))
    return {"repository": BASE, "packages": sorted(selected.values(), key=lambda item: item["name"])}


def fetch_one(item, downloads):
    filename = item["filename"]
    if Path(filename).name != filename or not re.fullmatch(r"[0-9a-f]{64}", item["sha256sum"]):
        raise RuntimeError("Invalid package lock entry")
    path = downloads / filename
    if not path.exists():
        contents = download(BASE + filename)
        if hashlib.sha256(contents).hexdigest() != item["sha256sum"]:
            raise RuntimeError(f"SHA-256 mismatch: {filename}")
        temporary = path.with_suffix(path.suffix + ".part")
        with temporary.open("xb") as output:
            output.write(contents)
        temporary.rename(path)
    if hashlib.file_digest(path.open("rb"), "sha256").hexdigest() != item["sha256sum"]:
        raise RuntimeError(f"SHA-256 mismatch: {filename}")
    print(f"Verified {filename}", flush=True)
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--lock", required=True, type=Path)
    parser.add_argument("--locked", action="store_true")
    args = parser.parse_args()
    if args.lock.exists():
        lock = json.loads(args.lock.read_text())
        if lock["repository"] != BASE:
            raise RuntimeError("Unsupported SDK repository")
    elif args.locked:
        raise RuntimeError("Missing SDK lock")
    else:
        lock = resolve()
        args.lock.parent.mkdir(parents=True, exist_ok=True)
        with args.lock.open("x") as output:
            json.dump(lock, output, indent=2)
            output.write("\n")
    downloads = args.root / "downloads"
    sdk = args.root / "sdk"
    downloads.mkdir(parents=True, exist_ok=True)
    sdk.mkdir(parents=True, exist_ok=True)
    # bsdtar understands zstd and refuses archive paths outside the target.
    # List before extraction as an additional check against absolute/traversal paths.
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
        for path in executor.map(lambda item: fetch_one(item, downloads), lock["packages"]):
            names = subprocess.check_output(["bsdtar", "-tf", str(path)], text=True).splitlines()
            for name in names:
                parts = PurePosixPath(name).parts
                if name.startswith("/") or ".." in parts:
                    raise RuntimeError(f"Unsafe archive member: {name}")
            subprocess.run(["bsdtar", "-xf", str(path), "-C", str(sdk), "--include=mingw64/*"], check=True)
    print(f"SDK ready: {sdk / 'mingw64'}")


if __name__ == "__main__":
    main()
