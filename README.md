<p align="center"><img src="docs/header.webp" alt="Flashes of Chaos MULTIPLAYER" width="720"></p>

# FOCMP

FOCMP adds three-player online co-op to [Flashes of Chaos](https://store.steampowered.com/app/3399100/) through Steam lobbies. It runs on Windows and on Linux or Steam Deck through Proton. The latest supported game version is **v1.0.02**.

FOCMP doesn't include the game: you need your own copy of Flashes of Chaos on Steam to use it.

It's an unofficial, free community mod and still experimental. It is not maintained or officially supported by [Brainburn Studio](https://brainburnstudio.com/).

## Install

> [!WARNING]
> Antivirus software may flag FOCMP, because it loads through a `version.dll` next to the game, a trick some
> malware also uses. Don't trust compiled code blindly: the source is in this repository, releases are built from it
> by [GitHub Actions](https://github.com/ojasookert/focmp/actions), and you can review it or
> [build it yourself](#build).

- Start the game once through Steam and quit it.
- Download the installer for your system from the [latest release](https://github.com/ojasookert/focmp/releases/latest): the `.exe` for Windows, the `.AppImage` for Linux and Steam Deck.
- Run it and press **Install**, then launch the game from Steam as usual.
- Run the installer again whenever you want to update or uninstall. Your saves and settings are kept either way.

<p align="center"><img src="docs/installer.webp" alt="FOCMP installer" width="540"></p>

If you'd rather install by hand, copy the DLLs and `focmp.ini` from the release ZIP next to `Flashes_of_Chaos.exe`. On Linux, also set the game's launch options in Steam to `WINEDLLOVERRIDES="version=n,b" %command%`.

## Play

Open **COOP**, choose **Host** and invite your friends. Everyone needs their own copy of the game, matching game and FOCMP versions, and has to join before the run starts.

Mod settings live in `focmp.ini` in the game folder. Modded saves are kept apart from your normal ones.

If something goes wrong, [open an issue](https://github.com/ojasookert/focmp/issues) with your FOCMP version, operating system, whether you were hosting or joining, and what happened. Include steps to reproduce it, any error messages, and relevant log files or screenshots if available.

## Known issues

- You can't join a run in progress, reconnect after dropping out, or take over when the host leaves.
- While the host is in the shop or lab, other players can't damage targets in the hub.
- The ESC menu doesn't pause the game; the world keeps going for everyone.

## Build

You need Linux with Rustup, MinGW-w64 GCC and Python 3. Run `scripts/build.sh`, then `python3 scripts/package_release.py`. Pushing a `vX.Y.Z` tag builds and publishes a release through GitHub Actions.

## License

The mod and installer are MIT licensed (see [LICENSE](LICENSE)). Flashes of Chaos and its assets belong to Brainburn Studio, including the logo and art shown in `docs/header.webp`, `docs/installer.webp` and the installer's banner and icon. That art is used with Brainburn Studio's permission, and the installer includes it. The MULTIPLAYER lettering is the mod's own, made by `scripts/make_art.py`.

## Donate

FOCMP is free and always will be. If you'd like to support its development, you can buy me a coffee at https://ko-fi.com/ojasookert. Please note that donations go to the mod's development, not to Brainburn Studio.
