# keydock

keydock is a planned Windows 10 and 11 x64 tray app that blocks a laptop's built-in keyboard while an allowlisted external keyboard is connected. For now it runs in the notification area without changing keyboard input.

## Run keydock

With a current stable Rust toolchain installed, run this command from the repository root:

```sh
cargo run
```

Look for the keyboard icon in the Windows notification area (check hidden icons if necessary). Hover over it to see `keydock: inactive`. Right-click and choose **Quit** to exit. There is no application window. Only one keydock runs per Windows session: if keydock is already running, a second start exits at once with no message.

## Update notice

About 60 seconds after it starts, and then every 24 hours, keydock asks GitHub for the latest release of this repository (`api.github.com/repos/<owner>/keydock/releases/latest`, User-Agent `keydock/<version>`). It sends nothing else. If the release is newer than the running version, the tray menu shows **Update available: vX.Y.Z**, which opens the release page, and the tooltip says so too. If the check fails, the menu shows a disabled **Update check failed** item with the reason. keydock never downloads or installs anything itself.

## Check the Quit action

On an unlocked Windows desktop with Explorer running and Python 3.8 or newer installed, run:

```sh
cargo build --locked
python tests/tray_quit.py target/debug/keydock.exe
```

Quit any running keydock first. The check holds a lock that keydock cannot open, as an elevated keydock's lock would be, and expects keydock to exit. It also starts a second keydock and expects it to exit. Then it opens a native menu for its own keydock process. Leave the menu alone until the check finishes. This desktop check is separate from `cargo test`.

## Driver

This version does not use a driver yet. The [v1 design](docs/DESIGN.md) uses the [Interception](https://github.com/oblitum/Interception) driver by Francisco Lopes. Its driver and installer are free for non-commercial use only, and its user-mode library is LGPL 3.0. Commercial use needs a license from the author.

The release zip bundles Interception 1.0.1: `interception.dll`, `install-interception.exe`, and the LGPL and GPL texts. To install the driver, open a terminal as administrator in the unzipped folder, run `install-interception.exe /install`, and restart Windows. To remove it, run `install-interception.exe /uninstall` and restart. The repository itself contains no Interception files.

## License

keydock is MIT-licensed; see [LICENSE](LICENSE). The bundled Interception files keep their own terms (see Driver above).

## Release

Set `version` in `Cargo.toml`, commit, then push a matching tag (for example `v0.2.0`). The `release` workflow checks that the tag matches the version, builds keydock, runs `scripts/package-release.sh` to download Interception at a pinned SHA-256, and publishes `keydock-<tag>-x64.zip` as a GitHub release. To build the zip locally, run `cargo build --release --locked` and then `bash scripts/package-release.sh v0.1.0`.
