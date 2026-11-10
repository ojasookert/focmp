#!/usr/bin/env python3
"""Package the native installer; Linux tooling is downloaded with pinned hashes."""
import hashlib, json, os, shutil, stat, subprocess, sys, urllib.request
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / 'dist'
version = json.loads((DIST / 'manifest.json').read_text())['version']
if sys.platform == 'win32':
    src = ROOT / 'installer/target/release/focmp-installer.exe'
    out = DIST / f'FOCMP-Installer-{version}-Windows-x86_64.exe'
    shutil.copyfile(src, out)
else:
    cache = ROOT / '.cache/installer-tools'
    cache.mkdir(parents=True, exist_ok=True)
    def fetch(name, url, sha):
        p = cache / name
        if not p.exists() or hashlib.sha256(p.read_bytes()).hexdigest() != sha:
            with urllib.request.urlopen(url, timeout=120) as r:
                data = r.read(32 * 1024 * 1024 + 1)
            if hashlib.sha256(data).hexdigest() != sha:
                raise RuntimeError(f'Checksum mismatch for {name}')
            p.write_bytes(data)
        p.chmod(0o755)
        return p
    tool = fetch('appimagetool.AppImage',
        'https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage',
        'ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0')
    runtime = fetch('runtime-x86_64',
        'https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64',
        '2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d')
    app = DIST / 'FOCMP.AppDir'
    if app.exists(): shutil.rmtree(app)
    (app / 'usr/bin').mkdir(parents=True)
    shutil.copy2(ROOT / 'installer/target/release/focmp-installer', app / 'usr/bin/focmp-installer')
    (app / 'AppRun').symlink_to('usr/bin/focmp-installer')
    shutil.copyfile(ROOT / 'installer/assets/focmp.png', app / 'focmp.png')
    (app / '.DirIcon').symlink_to('focmp.png')
    (app / 'focmp.desktop').write_text('[Desktop Entry]\nType=Application\nName=FOCMP Installer\nComment=Unofficial Flashes of Chaos co-op mod\nExec=focmp-installer\nIcon=focmp\nCategories=Game;Utility;\nTerminal=false\n')
    notices = app / 'usr/share/doc/focmp'
    notices.mkdir(parents=True)
    shutil.copyfile(ROOT / 'LICENSE', notices / 'LICENSE')
    out = DIST / f'FOCMP-Installer-{version}-Linux-x86_64.AppImage'
    subprocess.run([str(tool), '--appimage-extract-and-run', '--no-appstream', '--runtime-file', str(runtime), str(app), str(out)], env={**os.environ, 'ARCH':'x86_64'}, check=True)
    out.chmod(0o755)
print(f'{out}: {out.stat().st_size:,} bytes, SHA256 {hashlib.sha256(out.read_bytes()).hexdigest()}')
