# keydock design

keydock blocks the laptop's built-in keyboard while an allowlisted external keyboard is connected, and restores it when the last one disconnects.

## Scope

- Platform: Windows 10 and 11, x64 only.
- Language: Rust.
- Form: tray app. No service, no settings window.

## Blocking mechanism

Windows has no supported user-mode API that blocks one keyboard and passes another. A low-level hook (`WH_KEYBOARD_LL`) cannot see the source device. Raw Input sees the device but cannot block the key.

keydock uses the [Interception](https://github.com/oblitum/Interception) keyboard filter driver. It can identify each device and drop its keys.

Rejected options:

- Hook plus Raw Input correlation: the hook runs before `WM_INPUT` arrives, so the result is not reliable.
- Disable the device through SetupAPI: the disable stays after a crash, and PS/2 keyboards often need a reboot to change state. This breaks the failsafe rules below.
- Own KMDF filter driver: needs an EV certificate and Microsoft attestation signing. Too much work for v1.

### Driver install

The release zip bundles Interception 1.0.1: the x64 `interception.dll`, `install-interception.exe`, and the license texts. The user runs the installer once (admin rights and a reboot). The free license does not allow a silent install embedded in keydock's own installer, so keydock never runs it on its own.

The Interception non-commercial license allows redistributing the driver and installer "once communication with drivers happen solely by use of the library and its API". keydock therefore talks to the driver only through `interception.dll`, never through direct device I/O.

If the driver is missing at startup, keydock stays inactive and shows an error toast that points to `install-interception.exe` in keydock's folder.

## Devices

### Trigger: allowlisted external keyboards

- Only keyboards on the allowlist trigger the block. Receiver dongles, YubiKeys and headsets often report as keyboards, so "any external keyboard" would block by mistake.
- keydock identifies a keyboard by VID:PID, read from the Interception hardware ID (`HID\VID_xxxx&PID_xxxx...`). This matches the same model on any port and after Bluetooth re-pairing. Two units of the same model both match.
- The user edits the allowlist in a tray submenu that lists connected keyboards with checkboxes. A keyboard with several interfaces (same VID:PID) gets one checkbox. Built-in keyboards (below) get none: they are always connected, so checking one would keep the built-in keyboard blocked.

### Target: the built-in keyboard only

- keydock blocks only the built-in keyboard, never "every keyboard not on the allowlist".
- keydock treats every keyboard that Windows places in the PC's own device container as built-in (`DEVPKEY_Device_InLocalMachineContainer`, read through SetupAPI and matched to the Interception device by first hardware ID), and blocks all of them. A hardware-ID rule failed: on the development laptop the keys come from a USB HID device (`HID\VID_048D&PID_C197...Col04`, an ITE controller), while the `ACPI\` keyboard sends nothing. Blocking a silent internal keyboard as well costs nothing.
- If the container check fails, keydock finds no built-in keyboard; it never falls back to the hardware-ID rule. The user can override the choice in the tray menu (not built yet).
- If keydock cannot find a built-in keyboard at startup, it stays inactive and shows an error toast. The tray menu also shows the error as a disabled item.

### Connect and disconnect

- keydock listens for keyboard arrival and removal (`CM_Register_Notification` with `GUID_DEVINTERFACE_KEYBOARD`).
- Windows' device list (SetupAPI) decides which keyboards are present. The driver's list only supplies the device numbers to block: it lags arrivals and removals, and it stops listing new keyboards once its slots run out.
- The driver drops all input from a keyboard without a slot. So an allowlisted keyboard triggers the block only if every one of its interfaces has a slot; otherwise the menu says so and the built-in keyboard stays on. Without this rule, a keyboard without a slot locked the user out (seen on the development laptop).
- At startup and after each report keydock scans again every 250 ms for 5 s, because the driver's list lags the report (under 2 s on the development laptop).
- An arrival report for an allowlisted keyboard ends **Re-enable until next connect**, even if no scan saw the keyboard leave.
- If a rescan fails, keydock stops blocking until the next keyboard change.
- The built-in keyboard stays blocked while at least one allowlisted keyboard is connected. keydock restores it when the last one disconnects.

## Failsafe

A user must never be left with no working keyboard.

1. Reversible block: when keydock's process dies, the driver drops its filters. Verified on the development laptop: a keydock killed while blocking ended in about 0.2 s, and the built-in keyboard worked at once.
2. keydock restores the built-in keyboard at every startup before it evaluates state. It clears only the built-in keyboards' filters, so it does not break another Interception tool.
3. keydock sets the filter only on the built-in keyboards. A hung keydock (alive but not reading keys) can freeze only them; other keyboards never pass through keydock.
4. While blocking, keydock passes key-up events through, so a key held down when the block starts is not left down in Windows.
5. Tray menu items that work with the touchpad alone:
   - **Pause keydock**: the built-in keyboard stays enabled until the user chooses **Resume**, also through reconnects. Use when the external keyboard is unreliable.
   - **Re-enable until next connect**: the built-in keyboard is enabled now; keydock blocks it again the next time an allowlisted keyboard connects. The item is always enabled: muda disables items without graying them, so a disabled item would look clickable.

## UI

- Crates: `tray-icon` for the tray and menu, `windows` for Win32 calls.
- Two tray icons: blocked (the keyboard with a red slash) and not blocked (the plain keyboard). Paused, suspended and inactive all use the plain icon; the tooltip names the exact state.
- Toast notifications only for errors found at startup (missing DLL or driver, no built-in keyboard found). No toast for normal block and restore.
- A toast is a tray balloon (`Shell_NotifyIconW` with `NIF_INFO`), which Windows 10 and 11 show as a toast without an app ID or Start menu shortcut; a portable zip has neither. The balloon names tray-icon's private icon ID, so the `tray-icon` pin must stay exact. If the shell refuses the balloon, keydock shows the error in a dialog instead.

## Config

- File: `%APPDATA%\keydock\config.toml` (from `SHGetKnownFolderPath(FOLDERID_RoamingAppData)`, so no `dirs` crate).
- keydock writes a temporary file, flushes it to disk, and renames it, so a crash or power cut never leaves a half-written config. If the file exists but cannot be read, keydock starts with an empty allowlist and does not save, so it never overwrites the user's file.
- Holds: the allowlist (VID:PID list), the built-in keyboard override, the paused flag.

## Autostart

- An `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value. Per-user, no admin rights.
- A **Start at login** checkbox in the tray menu adds or removes the value. The value is the quoted path of the running `keydock.exe`; the checkbox shows checked only when the value matches it, so a moved folder shows unchecked instead of a value that starts nothing.

## Updates

- Notify only: keydock never downloads or replaces its own files, so it needs no update signature. If install-on-click is added later, sign each release with Ed25519 and embed the public key.
- A background thread asks `api.github.com` for the latest release 60 seconds after startup, then every 24 hours. The delay keeps startup, and the short-lived keydocks in the desktop check, free of network requests. GitHub allows 60 unauthenticated requests per hour per address.
- The release tag must be `vMAJOR.MINOR.PATCH`; keydock compares it with its own `Cargo.toml` version as numbers. HTTP 404 means no release yet and counts as up to date.
- A newer release adds an **Update available** item at the top of the tray menu and a note in the tooltip. A failed check adds an item with the reason, so the failure is visible without a dialog. It opens the release page like the update item: muda cannot gray out an item it disables.
- The repository comes from the `repository` field in `Cargo.toml`; a unit test checks it points at GitHub.

## Release

- A portable zip attached to GitHub Releases: `keydock.exe`, the bundled Interception files, `NOTICE.txt`, keydock's MIT license, and the LGPL and GPL texts.
- The `release` workflow builds it when a `v*` tag is pushed and fails if the tag does not match the `Cargo.toml` version.
- `scripts/package-release.sh` downloads Interception and the GPL text at pinned URLs and checks their SHA-256 before packaging. The repository holds no third-party binaries.
- winget or MSI: not in v1.

## License

- keydock: MIT.
- Interception: its user-mode library is LGPL 3.0; its driver and installer are free for non-commercial use only. keydock loads the DLL at run time, so users can replace it (LGPL section 4). Each release carries `NOTICE.txt` plus the LGPL and GPL texts. The README must state the driver terms.
- Bundling makes any redistribution of the release zip subject to Interception's non-commercial terms. Commercial use needs a license from the author.

## Open checks

These are facts to verify during the build, not open decisions:

- Does the Interception client need admin rights? Autostart through the Run key assumes it does not. If it does, autostart needs a Task Scheduler task instead.
  - Answered: no. A non-elevated keydock reads every hardware ID and blocks keys (`src/interception.rs`).
- Is the reported 10-device-slot exhaustion after many hot-plug cycles still present? If yes, document it in the README.
  - Answered: yes. After three Keychron reconnects the driver gave it no slot while six slots were empty, and the Keychron typed nothing with keydock not running; Windows listed it as started. That slot numbers are never reused before a restart is inferred from the counts, not read in the driver source. Documented in the README.
- What is the built-in keyboard's hardware ID on the development laptop? This confirms the auto-detection rule.
  - Answered, and the first answer was wrong. A pass-through test that logged the source device of each key showed that the laptop keys come from the ITE USB device `HID\VID_048D&PID_C197&REV_1800&Col04`, not from the `ACPI\VEN_IDEA&DEV_0100` device. That disproved the hardware-ID rule; see Target above.
