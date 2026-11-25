# keydock

keydock is a planned Windows 10 and 11 x64 tray app that blocks a laptop's built-in keyboard while an allowlisted external keyboard is connected. This scaffold only prints a status message and exits. It does not create a tray icon or change keyboard input.

## Run the scaffold

With a current stable Rust toolchain installed, run this command from the repository root:

```sh
cargo run
```

Expected program output:

```text
keydock is inactive. Keyboard input is unchanged.
```

No driver is required for the scaffold. The [v1 design](docs/DESIGN.md) calls for the [Interception](https://github.com/oblitum/Interception) driver, which is free for non-commercial use only. Its user-mode library is LGPL. Neither is bundled here.
