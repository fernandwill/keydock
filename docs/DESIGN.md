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
- The user edits the allowlist in a tray submenu that lists connected keyboards with checkboxes.

### Target: the built-in keyboard only

- keydock blocks only the built-in keyboard, never "every keyboard not on the allowlist".
- keydock detects the built-in keyboard from its hardware ID (`ACPI\...` for PS/2/i8042, or an I2C HID ID). The user can override the choice in the tray menu.
- If keydock cannot find a built-in keyboard, it stays inactive and shows an error toast.

### Connect and disconnect

- keydock listens for keyboard arrival and removal (`RegisterDeviceNotification` with `GUID_DEVINTERFACE_KEYBOARD`).
- The built-in keyboard stays blocked while at least one allowlisted keyboard is connected. keydock restores it when the last one disconnects.

## Failsafe

A user must never be left with no working keyboard.

1. Reversible block: Interception passes all input through when keydock's process dies, so a crash restores the built-in keyboard.
2. keydock restores the built-in keyboard at every startup before it evaluates state.
3. Tray menu items that work with the touchpad alone:
   - **Pause keydock**: the built-in keyboard stays enabled until the user chooses **Resume**, also through reconnects. Use when the external keyboard is unreliable.
   - **Re-enable until next connect**: the built-in keyboard is enabled now; keydock blocks it again the next time an allowlisted keyboard connects.

## UI

- Crates: `tray-icon` for the tray and menu, `windows` for Win32 calls.
- Two tray icons: active (built-in keyboard blocked) and inactive. The paused state shows in the icon or tooltip.
- Toast notifications only for errors (missing driver, no built-in keyboard found). No toast for normal block and restore.

## Config

- File: `%APPDATA%\keydock\config.toml` (from `dirs::config_dir()`).
- Holds: the allowlist (VID:PID list), the built-in keyboard override, the paused flag.

## Autostart

- An `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value. Per-user, no admin rights.
- A **Start at login** checkbox in the tray menu adds or removes the value.

## Release

- A portable zip attached to GitHub Releases: `keydock.exe`, the bundled Interception files, `NOTICE.txt`, and the LGPL and GPL texts.
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
- Is the reported 10-device-slot exhaustion after many hot-plug cycles still present? If yes, document it in the README.
- What is the built-in keyboard's hardware ID on the development laptop? This confirms the auto-detection rule.
  - Answered: the built-in keyboard is a PS/2 device with an `ACPI\` hardware ID, so the rule matches it. The same laptop also lists a receiver dongle, a vendor's virtual keyboard, and an internal USB controller (`HID\VID_048D`, likely the backlight) as keyboards. The rule correctly skips the internal controller, and the allowlist keeps all three from triggering a block.
