#!/usr/bin/env python3
"""Verify the official Ubuntu Base archive before extracting a build-only root."""
import hashlib, subprocess, sys, urllib.request
from pathlib import Path
root=Path(sys.argv[1]).resolve()
base="https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/"
name="ubuntu-base-24.04.4-base-amd64.tar.gz"
checksums=urllib.request.urlopen(base+"SHA256SUMS",timeout=60).read().decode()
digest=next(line.split()[0] for line in checksums.splitlines() if line.split()[-1].lstrip("*")==name)
archive=root/name
if not archive.exists():
    with urllib.request.urlopen(base+name,timeout=120) as response,archive.open("xb") as output:
        while chunk:=response.read(1024*1024):output.write(chunk)
with archive.open("rb") as f:
    if hashlib.file_digest(f,"sha256").hexdigest()!=digest:raise RuntimeError("Ubuntu Base checksum mismatch")
directory=root/"rootfs"
if not directory.exists():
    directory.mkdir()
    subprocess.run(["tar","--no-same-owner","--no-same-permissions","-xzf",str(archive),"-C",str(directory)],check=True)
    (directory/"usr/sbin/policy-rc.d").write_text("#!/bin/sh\nexit 101\n")
    (directory/"usr/sbin/policy-rc.d").chmod(0o755)
    (directory/"tmp").chmod(0o1777)
print(directory)
