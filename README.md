# keydock

keydock is a planned Windows 10 and 11 x64 tray app that blocks a laptop's built-in keyboard while an allowlisted external keyboard is connected. For now it runs in the notification area without changing keyboard input.

## Run keydock

With a current stable Rust toolchain installed, run this command from the repository root:

```sh
cargo run
```

Look for the keyboard icon in the Windows notification area (check hidden icons if necessary). Hover over it to see `keydock: inactive`. Right-click and choose **Quit** to exit. There is no application window.

No driver is required for this version. The [v1 design](docs/DESIGN.md) calls for the [Interception](https://github.com/oblitum/Interception) driver, which is free for non-commercial use only. Its user-mode library is LGPL. Neither is bundled here.
