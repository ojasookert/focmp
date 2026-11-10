#!/usr/bin/env python3
"""Exercise the built installer against disposable folders. No game or Rust tests."""
import argparse, copy, hashlib, json, os, pathlib, subprocess, tempfile, zipfile
p=argparse.ArgumentParser();p.add_argument('binary');p.add_argument('--skip-steam',action='store_true');args=p.parse_args()
binary=str(pathlib.Path(args.binary).resolve());root=pathlib.Path(__file__).resolve().parents[1]
source=json.loads((root/'dist/manifest.json').read_text());payload=root/'dist/bundle/payload.zip'
def digest(b):return hashlib.sha256(b).hexdigest()
with tempfile.TemporaryDirectory(prefix='focmp-installer-check-') as tmp:
 t=pathlib.Path(tmp);game=t/"Game ü space ' quote";game.mkdir();exe=game/'Flashes_of_Chaos.exe';exe.write_bytes(b'disposable supported executable fixture');(game/'data.win').write_bytes(b'disposable data fixture')
 # Exercise the current two-file compatibility check, even with an older local bundle.
 m=copy.deepcopy(source)
 m['supported_game_sha256']=[digest(exe.read_bytes())]
 m['game_versions']=[{'name':'installer fixture','exe_sha256':digest(exe.read_bytes()),'data_sha256':digest((game/'data.win').read_bytes())}]
 mp=t/'manifest.json';mp.write_text(json.dumps(m))
 def run(command='install',ok=True,extra=(),steam=False,manifest=mp,archive=payload):
  cmd=[binary,command,'--game-dir',str(game),'--manifest',str(manifest),'--package',str(archive)]
  if not steam:cmd+=['--no-proton-config']
  r=subprocess.run(cmd+list(extra),capture_output=True,text=True,timeout=60)
  assert (r.returncode==0)==ok,(cmd,r.returncode,r.stdout,r.stderr)
  return r.stdout+r.stderr
 original=b'pre-existing proxy DLL';(game/'version.dll').write_bytes(original)
 settings=b'[network]\ntransport = tcp\n';(game/'focmp.ini').write_bytes(settings)
 assert 'installed' in run();assert (game/'focmp.ini').read_bytes()==settings
 run();assert (game/'.focmp/backup/version.dll').read_bytes()==original
 assert 'removed' in run('uninstall');assert (game/'version.dll').read_bytes()==original
 assert (game/'focmp.ini').read_bytes()==settings and not (game/'focmp.dll').exists()
 (game/'version.dll').unlink();(game/'focmp.ini').unlink()
 run();(game/'focmp.ini').write_bytes(settings);run('uninstall');assert (game/'focmp.ini').read_bytes()==settings
 run();installed=(game/'focmp.dll').read_bytes();(game/'focmp.dll').write_bytes(b'external modification')
 run('uninstall',ok=False);run(ok=False);assert (game/'focmp.dll').read_bytes()==b'external modification'
 (game/'focmp.dll').write_bytes(installed);run('uninstall')
 # A failure after the first DLL replacement must roll back, and a retry must work.
 (game/'focmp.ini').unlink();(game/'version.dll').write_bytes(original)
 (game/'.focmp.ini.focmp-tmp').write_bytes(b'occupied staging path')
 run(ok=False);assert (game/'version.dll').read_bytes()==original and not (game/'focmp.dll').exists()
 (game/'.focmp.ini.focmp-tmp').unlink();run();run('uninstall')
 assert (game/'version.dll').read_bytes()==original;(game/'version.dll').unlink()
 # Wrong game, wrong checksum, incomplete payload and traversal all fail before replacing files.
 badpath=t/'bad.json'
 # Each file must match the same supported version; the legacy exe list cannot bypass it.
 for field in ['exe_sha256','data_sha256']:
  bad=copy.deepcopy(m);bad['game_versions'][0][field]='0'*64;badpath.write_text(json.dumps(bad))
  assert 'not supported' in run(ok=False,manifest=badpath)
  assert not (game/'version.dll').exists() and not (game/'focmp.dll').exists()
 bad=copy.deepcopy(m)
 bad['game_versions']=[{**m['game_versions'][0],'data_sha256':'0'*64},{**m['game_versions'][0],'exe_sha256':'0'*64}]
 badpath.write_text(json.dumps(bad));assert 'not supported' in run(ok=False,manifest=badpath)
 # Pre-game_versions manifests still use the executable-only compatibility list.
 legacy=copy.deepcopy(m);legacy.pop('game_versions');legacy_path=t/'legacy.json';legacy_path.write_text(json.dumps(legacy))
 run(manifest=legacy_path);run('uninstall')
 legacy['supported_game_sha256']=['0'*64];legacy_path.write_text(json.dumps(legacy))
 assert 'not supported' in run(ok=False,manifest=legacy_path)
 broken=t/'broken.zip';broken.write_bytes(b'x'*m['package']['size']);run(ok=False,archive=broken)
 for filename in ['../escape','Flashes_of_Chaos.exe','version.dll']:
  zpath=t/'hostile.zip'
  with zipfile.ZipFile(zpath,'w') as z:z.writestr(filename,b'bad')
  bad=copy.deepcopy(m);bad['package']['sha256']=digest(zpath.read_bytes());bad['package']['size']=zpath.stat().st_size;badpath.write_text(json.dumps(bad))
  run(ok=False,manifest=badpath,archive=zpath);assert not (t/'escape').exists()
 assert not (game/'focmp.dll').exists() and exe.read_bytes()==b'disposable supported executable fixture'
 # Symlinks must not cause writes outside the installation.
 if os.name!='nt':
  outside=t/'outside';outside.write_bytes(b'unchanged');(game/'version.dll').symlink_to(outside)
  run(ok=False);assert outside.read_bytes()==b'unchanged';(game/'version.dll').unlink()
 if os.name!='nt' and not args.skip_steam:
  steam=t/'Steam';(steam/'steamapps').mkdir(parents=True);sr=['--steam-root',str(steam)]
  # No prefix yet: refused before any file changes.
  out=run(ok=False,steam=True,extra=sr);assert 'Start the game once' in out and not (game/'focmp.dll').exists(),out
  reg=steam/'steamapps/compatdata/3399100/pfx/user.reg';reg.parent.mkdir(parents=True)
  key='[Software\\\\Wine\\\\AppDefaults\\\\Flashes_of_Chaos.exe\\\\DllOverrides]'
  head='WINE REGISTRY Version 2\n;; All keys relative to REGISTRY\\\\User\\\\S-1-5-21-0-0-1000\n\n#arch=win64\n\n[Software\\\\Wine\\\\AppDefaults\\\\Other.exe\\\\DllOverrides] 1793301782\n#time=1dd67daeaad61e4\n"version"="builtin"\n\n'
  ours='"version"="native,builtin"'
  cases=[head, head+key+' 1793301782\n#time=1dd\n"d3d9"="native"\n\n', head+key+' 1793301782\n"VERSION"="builtin"\n\n']
  for before in cases:
   reg.write_text(before);run(steam=True,extra=sr);after=reg.read_text()
   section=after[after.index(key):].split('\n\n')[0]
   assert ours in section and '"version"="builtin"' in after.split(key)[0],after
   assert '"d3d9"="native"' in section or 'd3d9' not in before
   run(steam=True,extra=sr);assert reg.read_text()==after # idempotent
   run('uninstall');assert reg.read_text()==before,(before,reg.read_text())
  # The prefix recreated after install: a reinstall puts the override back; uninstall then removes it.
  reg.write_text(head);run(steam=True,extra=sr);reg.write_text(head);run(steam=True,extra=sr);assert ours in reg.read_text()
  run('uninstall');assert reg.read_text()==head
  # A user changed the value after install: uninstall leaves it.
  run(steam=True,extra=sr);reg.write_text(reg.read_text().replace(ours,'"version"="native"'));run('uninstall')
  assert '"version"="native"' in reg.read_text();reg.write_text(head)
  print('Proton prefix override: insert, keep other values, idempotence, recreated prefix and restoration passed.')
 print('Install/update/uninstall, backups, settings, modified files, game mismatch, archive integrity and path safety passed.')
