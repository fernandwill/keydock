# keydock

keydock is a Windows 10 and 11 x64 tray app that blocks a laptop's built-in keyboard while an allowlisted external keyboard is connected. It reacts within a few seconds when you plug a keyboard in or out.

## Run keydock

With a current stable Rust toolchain installed, run this command from the repository root:

```sh
cargo run
```

Look for the keyboard icon in the Windows notification area (check hidden icons if necessary). Hover over it to see its state: `keydock: blocking the built-in keyboard`, `keydock: inactive`, `keydock: paused`, or `keydock: built-in keyboard on until the next connect`. Right-click to see the **Keyboards** submenu. It lists the connected keyboards by hardware ID. Every keyboard that Windows counts as part of the PC is marked `(built-in)` and cannot be checked; keydock blocks all of them. Each external keyboard model has a checkbox labeled with its VID:PID; check the keyboards that should trigger the block. The block starts or stops at once when you click. keydock saves the checked list to `%APPDATA%\keydock\config.toml` when you click. If that file cannot be read, the menu says so and keydock does not overwrite it. If the Interception DLL or driver is missing, one disabled item says so instead of the submenu. **Pause keydock** keeps the built-in keyboard on until you click it again, also after a restart. **Re-enable until next connect** turns the built-in keyboard on until an allowlisted keyboard is plugged in again. Choose **Quit** to exit; the built-in keyboard works again at once, and also if keydock crashes or is killed. There is no application window. Only one keydock runs per Windows session: if keydock is already running, a second start exits at once with no message.

**Known limit:** the Interception driver hands out a limited number of device slots per Windows session, and every keyboard connection uses new ones (the Keychron used two per connection on the development laptop). When they run out, a newly connected keyboard is listed by Windows but types nothing, with or without keydock running. keydock then shows `<VID:PID> gets no input until Windows restarts (Interception device limit)` and does not block the built-in keyboard. Restart Windows to get the slots back.

## Update notice

About 60 seconds after it starts, and then every 24 hours, keydock asks GitHub for the latest release of this repository (`api.github.com/repos/<owner>/keydock/releases/latest`, User-Agent `keydock/<version>`). It sends nothing else. If the release is newer than the running version, the tray menu shows **Update available: vX.Y.Z**, which opens the release page, and the tooltip says so too. If the check fails, the menu shows an **Update check failed** item with the reason, which also opens the release page. keydock never downloads or installs anything itself.

## Check the Quit action

On an unlocked Windows desktop with Explorer running and Python 3.8 or newer installed, run:

```sh
cargo build --locked
python tests/tray_quit.py target/debug/keydock.exe
```

Quit any running keydock first. The check holds a lock that keydock cannot open, as an elevated keydock's lock would be, and expects keydock to exit. It also starts a second keydock and expects it to exit. Then it opens a native menu for its own keydock process. Leave the menu alone until the check finishes. This desktop check is separate from `cargo test`.

## Driver

keydock reads the keyboard list and blocks keys through the Interception driver. It loads `interception.dll` only from its own folder. For `cargo run`, copy the x64 `interception.dll` (from the release zip, or `library/x64` in Interception's `Interception.zip`) to `target\debug\`. The [v1 design](docs/DESIGN.md) uses the [Interception](https://github.com/oblitum/Interception) driver by Francisco Lopes. Its driver and installer are free for non-commercial use only, and its user-mode library is LGPL 3.0. Commercial use needs a license from the author.

The release zip bundles Interception 1.0.1: `interception.dll`, `install-interception.exe`, and the LGPL and GPL texts. To install the driver, open a terminal as administrator in the unzipped folder, run `install-interception.exe /install`, and restart Windows. To remove it, run `install-interception.exe /uninstall` and restart. The repository itself contains no Interception files.

## License

keydock is MIT-licensed; see [LICENSE](LICENSE). The bundled Interception files keep their own terms (see Driver above).

## Release

Set `version` in `Cargo.toml`, commit, then push a matching tag (for example `v0.2.0`). The `release` workflow checks that the tag matches the version, builds keydock, runs `scripts/package-release.sh` to download Interception at a pinned SHA-256, and publishes `keydock-<tag>-x64.zip` as a GitHub release. To build the zip locally, run `cargo build --release --locked` and then `bash scripts/package-release.sh v0.1.0`.
