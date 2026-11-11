#!/usr/bin/env python3
"""Package an existing build; does not compile or increment its version."""
import hashlib,json,os,re,shutil,zipfile
from pathlib import Path
root=Path(__file__).resolve().parents[1]
version=(root/'focmp/BUILD_VERSION').read_text().strip()
assert re.fullmatch(r'\d+\.\d+\.\d+',version)
files={name:(root/'dist'/name).read_bytes() for name in ('version.dll','focmp.dll','focmp.ini')}
assert version.encode() in files['focmp.dll'], 'Version counter and DLL differ: rebuild before packaging'
assert b'transport = steam' in files['focmp.ini'], 'Release must default to Steam'
assert b'console = false' in files['focmp.ini'], 'Release must keep the debug console off'
protocol=re.search(r'pub const PROTOCOL: u32 = (\d+)',(root/'focmp/src/net.rs').read_text())[1]
out=root/'dist'/f'FOCMP-{version}.zip'
with zipfile.ZipFile(out,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=9) as z:
    for name,data in files.items():z.writestr(name,data)
with zipfile.ZipFile(out) as z:
    assert z.testzip() is None
    assert set(z.namelist())==set(files)
    for name,data in files.items():assert z.read(name)==data
print(f'{out}: FOCMP {version}, protocol {protocol}, {out.stat().st_size:,} bytes')
print('Archive integrity and every packaged file verified.')

# Game versions this release supports: Flashes_of_Chaos.exe (GameMaker runner) and data.win (game code and assets)
# both identify a version; steam_build is shown to players. Add an entry when the mod is updated for a game update.
GAME_VERSIONS=[
    {'name':'v1.0.02','steam_build':26594650,
     'exe_sha256':'4e62680242c38f3122318916dab79b98e87ee396334d4f086076194abd1856aa',
     'data_sha256':'8cdd4236fd0efc26f3db1f58f28bb2146a7ac3ade17eed7671f0671ab1f28380'},
    {'name':'v1.0.01 hotfix','steam_build':26523032,
     'exe_sha256':'00a9b93b2a42e4be425b2856bebbe5b6651b7f5d3deeb38a11fb713a6db4a8c4',
     'data_sha256':'0310530d628a70ebe1c22db4be198761c81db9d65d3500581e07c0391f4f9681'},
]

manifest={
    'schema':1,'mod_id':'focmp','version':version,'protocol':int(protocol),
    'source':{'commit':os.environ.get('GITHUB_SHA',''),'run':os.environ.get('GITHUB_RUN_ID',''),'attempt':os.environ.get('GITHUB_RUN_ATTEMPT','')},
    'game_app_id':3399100,'game_executable':'Flashes_of_Chaos.exe',
    'supported_game_sha256':[v['exe_sha256'] for v in GAME_VERSIONS],   # read by installers before game_versions
    'game_versions':GAME_VERSIONS,
    'package':{'url':f'https://github.com/ojasookert/focmp/releases/download/v{version}/FOCMP-{version}.zip',
               'sha256':hashlib.sha256(out.read_bytes()).hexdigest(),'size':out.stat().st_size},
}
(root/'dist/manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
bundle=root/'dist/bundle';bundle.mkdir(exist_ok=True)
shutil.copyfile(out,bundle/'payload.zip')
shutil.copyfile(root/'dist/manifest.json',bundle/'manifest.json')
print(f'Wrote {out.name}, manifest.json and the offline installer bundle.')
