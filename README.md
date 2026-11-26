# keydock

keydock is a planned Windows 10 and 11 x64 tray app that blocks a laptop's built-in keyboard while an allowlisted external keyboard is connected. For now it runs in the notification area without changing keyboard input.

## Run keydock

With a current stable Rust toolchain installed, run this command from the repository root:

```sh
cargo run
```

Look for the keyboard icon in the Windows notification area (check hidden icons if necessary). Hover over it to see `keydock: inactive`. Right-click and choose **Quit** to exit. There is no application window. Only one keydock runs per Windows session: if keydock is already running, a second start exits at once with no message.

## Check the Quit action

On an unlocked Windows desktop with Explorer running and Python 3.8 or newer installed, run:

```sh
cargo build --locked
python tests/tray_quit.py target/debug/keydock.exe
```

The check also starts a second keydock and expects it to exit. Then it opens a native menu for its own keydock process. Leave the menu alone until the check finishes. This desktop check is separate from `cargo test`.

## Driver

No driver is required for this version. The [v1 design](docs/DESIGN.md) calls for the [Interception](https://github.com/oblitum/Interception) driver, which is free for non-commercial use only. Its user-mode library is LGPL. Neither is bundled here.
