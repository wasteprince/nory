#!/usr/bin/env python3
"""Overlay pinned RoscomVPN categories onto packaged Xray GeoData for both cores."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.request

SOURCES = {
    'geoip': {
        'url': 'https://github.com/hydraponique/roscomvpn-geoip/releases/download/202609120752/geoip.dat',
        'sha256': '307965566d484eb7cfaeda715b823d20a1e02f4578a5aa95fbaaedd84ddd1070',
    },
    'geosite': {
        'url': 'https://github.com/hydraponique/roscomvpn-geosite/releases/download/202604152235/geosite.dat',
        'sha256': '765b86e4b6aed5da1a206304b5500c7668687fa1df8e8322c8a4961e1b672190',
    },
}
LIMIT = 128 * 1024 * 1024

def digest(data):
    return hashlib.sha256(data).hexdigest()

def fields(data):
    cursor = 0
    def varint():
        nonlocal cursor
        value = 0
        for shift in range(0, 70, 7):
            if cursor >= len(data): raise ValueError('Truncated protobuf')
            byte = data[cursor]
            cursor += 1
            if shift == 63 and byte > 1: raise ValueError('Protobuf overflow')
            value |= (byte & 127) << shift
            if byte < 128: return value
        raise ValueError('Invalid protobuf varint')
    while cursor < len(data):
        key = varint()
        wire = key & 7
        if key >> 3 == 0: raise ValueError('Invalid protobuf field')
        if wire == 0:
            varint()
            continue
        size = varint() if wire == 2 else {1: 8, 5: 4}.get(wire)
        if size is None or cursor + size > len(data): raise ValueError('Invalid protobuf data')
        value = data[cursor:cursor + size]
        cursor += size
        if wire == 2: yield key >> 3, value

def entries(data):
    if len(data) > LIMIT: raise ValueError('GeoData exceeds size limit')
    result = {}
    for field, entry in fields(data):
        if field != 1: raise ValueError('Unexpected GeoData root field')
        tags = [value.decode('ascii').lower() for field, value in fields(entry) if field == 1]
        if len(tags) != 1 or not tags[0] or tags[0] in result: raise ValueError('Invalid or duplicate GeoData category')
        result[tags[0]] = entry
    if not result: raise ValueError('Empty GeoData database')
    return result

def encode(entries):
    output = bytearray()
    for tag in sorted(entries):
        entry = entries[tag]
        output.append(10)
        size = len(entry)
        while size >= 128:
            output.append((size & 127) | 128)
            size >>= 7
        output.append(size)
        output.extend(entry)
    return bytes(output)

def atomic_write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            output.close()
            temporary.chmod(0o644)
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)

def stage(directory, provenance):
    cache = Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'nory-geodata'
    cache.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for kind, source in SOURCES.items():
        cached = cache / (source['sha256'] + '.dat')
        data = cached.read_bytes() if cached.exists() else b''
        if digest(data) != source['sha256']:
            with urllib.request.urlopen(source['url'], timeout=60) as response:
                data = response.read(LIMIT + 1)
            if digest(data) != source['sha256']: raise ValueError(f'{kind}: SHA-256 mismatch')
            entries(data)
            atomic_write(cached, data)
        source_entries = entries(data)
        path = directory / (kind + '.dat')
        base = path.read_bytes()
        merged = entries(base)
        fallback = sorted(merged.keys() - source_entries.keys())
        merged.update(source_entries)
        output = encode(merged)
        atomic_write(path, output)
        manifest[kind] = {**source, 'base_sha256': digest(base), 'output_sha256': digest(output),
                          'roscomvpn_categories': sorted(source_entries), 'fallback_categories': fallback}
    atomic_write(provenance, (json.dumps(manifest, indent=2) + '\n').encode())

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('provenance', type=Path)
    args = parser.parse_args()
    stage(args.directory, args.provenance)
