#!/usr/bin/env python3
"""Stage the Tauri/MSVC client, without the legacy GTK runtime."""
import argparse, concurrent.futures, hashlib, importlib.util, json, shutil, subprocess, zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('legacy_bundle', ROOT / 'scripts/windows/bundle.py')
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build-root', type=Path, required=True)
    parser.add_argument('--stage', type=Path, required=True)
    args = parser.parse_args()
    stage, build = args.stage.resolve(), args.build_root.resolve()
    if stage.exists():
        raise RuntimeError('Refusing to overwrite an existing stage')
    downloads = build / 'downloads'
    downloads.mkdir(parents=True, exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
        artifacts = dict(executor.map(lambda name: bundle.fetch(name, downloads), bundle.ARTIFACTS))
    stage.mkdir(parents=True)
    shutil.copy2(ROOT / 'desktop/src-tauri/target/x86_64-pc-windows-msvc/release/nory.exe', stage / 'nory.exe')
    shutil.copy2(ROOT / 'target/x86_64-pc-windows-msvc/release/nory-helper.exe', stage / 'nory-helper.exe')
    shutil.copy2(ROOT / 'assets/icons/io.nory.NORY.ico', stage / 'nory.ico')
    shutil.copy2(ROOT / 'desktop/README.md', stage / 'README.md')
    licenses = stage / 'licenses'
    licenses.mkdir()
    shutil.copy2(ROOT / 'LICENSE', licenses / 'NORY-LICENSE')
    shutil.copy2(ROOT / 'assets/flags/README.md', licenses / 'flags-README.md')
    shutil.copy2(ROOT / 'assets/flags/SOURCE.md', licenses / 'flags-SOURCE.md')
    shutil.copy2(ROOT / 'assets/maps/README.md', licenses / 'world-map.md')
    frontend_licenses = licenses / 'frontend'
    frontend_licenses.mkdir()
    for dependency in ['vue', '@vue/shared', '@vue/reactivity', '@vue/runtime-core', '@vue/runtime-dom', '@lucide/vue', '@tauri-apps/api', '@tauri-apps/plugin-dialog', 'tailwindcss']:
        for license in (ROOT / 'desktop/node_modules' / dependency).glob('LICENSE*'):
            if license.is_file():
                shutil.copy2(license, frontend_licenses / (dependency.replace('/', '-') + '-' + license.name))
    (licenses / 'cores.lock.json').write_text(json.dumps(bundle.ARTIFACTS, indent=2) + '\n')
    for core in ['xray', 'sing-box', 'mihomo']:
        destination = stage / 'cores' / core
        destination.mkdir(parents=True)
        with zipfile.ZipFile(artifacts[core]) as archive:
            for name in archive.namelist():
                base = Path(name).name
                if base.lower().endswith('.exe'):
                    if core == 'xray' and base != 'xray.exe': continue
                    with (destination / f'{core}.exe').open('xb') as output: output.write(archive.read(name))
                elif base.lower().endswith('.dll') or base in ['geoip.dat', 'geosite.dat']:
                    with (destination / base).open('xb') as output: output.write(archive.read(name))
                elif base.upper().startswith(('LICENSE', 'COPYING')):
                    (licenses / f'{core}-{base}').write_bytes(archive.read(name))
        if not (destination / f'{core}.exe').is_file(): raise RuntimeError(f'Missing {core}')
    with zipfile.ZipFile(artifacts['wintun']) as archive:
        for core in ['sing-box', 'mihomo']:
            (stage / 'cores' / core / 'wintun.dll').write_bytes(archive.read('wintun/bin/amd64/wintun.dll'))
        (licenses / 'Wintun-LICENSE.txt').write_bytes(archive.read('wintun/LICENSE.txt'))
    plugin = build / 'nsis-plugin'
    plugin.mkdir(exist_ok=True)
    subprocess.run(['bsdtar', '-xf', str(artifacts['shell-plugin']), '-C', str(plugin)], check=True)
    shutil.copytree(plugin / 'Docs/ShellExecAsUser', licenses / 'ShellExecAsUser')
    # Resolve every imported DLL. MSVC CRT and WebView2 loader are statically linked.
    bundle.copy_dependencies(stage, build / 'unused-sdk', 'objdump')
    files = sorted(p for p in stage.rglob('*') if p.is_file())
    (stage / 'installed-files.json').write_text(json.dumps({str(p.relative_to(stage)): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}, indent=2) + '\n')
    files.append(stage / 'installed-files.json')
    uninstall = [f'Delete /REBOOTOK "$INSTDIR\\{str(p.relative_to(stage)).replace(chr(47), chr(92))}"' for p in files]
    for p in sorted((p for p in stage.rglob('*') if p.is_dir()), key=lambda p: len(p.parts), reverse=True):
        uninstall.append(f'RMDir "$INSTDIR\\{str(p.relative_to(stage)).replace(chr(47), chr(92))}"')
    (stage.parent / 'uninstall-files.nsh').write_text('\n'.join(uninstall) + '\n')
    print(stage)

if __name__ == '__main__': main()
