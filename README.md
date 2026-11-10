<p align="center"><img src="docs/header.webp" alt="Flashes of Chaos MULTIPLAYER" width="720"></p>

# FOCMP

Three-player co-op for [Flashes of Chaos](https://store.steampowered.com/app/3399100/), via Steam lobbies.
Windows and Linux / Steam Deck (Proton). For game version **v1.0.01 hotfix**.

Unofficial and free. Not supported by [Brainburn Studio](https://brainburnstudio.com/). Experimental.

## Install

1. Start the game once through Steam, then quit.
2. Run the installer from the [latest release](https://github.com/ojasookert/focmp/releases/latest)
   (`.exe` or `.AppImage`) and press **Install**.

<p align="center"><img src="docs/installer.webp" alt="FOCMP installer" width="540"></p>

Run it again to update or uninstall; saves are kept. Manual install: copy the ZIP's DLLs and INI next to
`Flashes_of_Chaos.exe`; on Proton add `WINEDLLOVERRIDES="version=n,b" %command%` to the launch options.

## Play

**COOP > Host > Invite friends.** Everyone needs the same mod version and must join before the run starts.
Settings are in `focmp.ini`. Modded saves are kept separate from your normal ones.

Not supported yet: joining mid-run, reconnecting, host migration.
[Report issues](https://github.com/ojasookert/focmp/issues) with your mod version and whether you hosted.

## Build

Linux with Rustup, MinGW-w64 GCC and Python 3: `scripts/build.sh`, then `python3 scripts/package_release.py`.
Tag `vX.Y.Z` to release through GitHub Actions.

## License

MIT for the mod and installer ([LICENSE](LICENSE)). The game and its assets belong to Brainburn Studio;
each player needs their own copy. That includes the Flashes of Chaos logo and art in `docs/header.webp` and in the
installer's banner and icon; the installer takes them from your Steam library cache or Steam's CDN and doesn't
bundle them. The MULTIPLAYER lettering is the mod's own (`scripts/make_art.py`).
