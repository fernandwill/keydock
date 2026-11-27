#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("keydock supports Windows only");

mod autostart;
mod config;
mod devices;
mod interception;
mod update;

use config::Config;
use devices::{Change, KeyboardWatch, Present};
use interception::{Blocker, Devices, Driver, Keyboard, ScanError};
use std::{
    cell::Cell,
    collections::HashMap,
    error::Error,
    io,
    path::PathBuf,
    process::ExitCode,
    sync::mpsc,
    time::{Duration, Instant},
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
};
use update::Outcome;
use windows::{
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, WPARAM,
        },
        System::Threading::{CreateMutexW, GetCurrentThreadId},
        UI::{
            Shell::{
                NIF_INFO, NIIF_ERROR, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
            },
            WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, KillTimer, MB_ICONERROR, MSG, MessageBoxW,
                PostThreadMessageW, SW_SHOWNORMAL, SetTimer, TranslateMessage, WM_APP, WM_TIMER,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

// The driver's keyboard list lags Windows' arrival and removal reports; the lag was under 2 s
// on the development laptop. keydock scans again at this interval for this long after a report.
const RESCAN_INTERVAL_MS: u32 = 250;
const RESCAN_WINDOW: Duration = Duration::from_secs(5);
// ShellExecuteW reports success with a value above this.
const SHELL_EXECUTE_MAX_ERROR: isize = 32;
// tray-icon 0.26.1 gives its only icon this private shell ID (see tests/tray_quit.py).
const TRAY_ICON_ID: u32 = 2;

fn main() -> ExitCode {
    if let Err(error) = run() {
        show_error(&format!("keydock could not continue: {error}"));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn show_error(message: &str) {
    unsafe { MessageBoxW(None, &HSTRING::from(message), w!("keydock"), MB_ICONERROR) };
}

/// Shows `message` as an error toast from the tray icon, or as a dialog if the shell refuses.
fn notify_error(tray: &TrayIcon, message: &str) {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(tray.window_handle()),
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_ERROR,
        ..Default::default()
    };
    copy_wide(&mut data.szInfoTitle, "keydock");
    copy_wide(&mut data.szInfo, message);
    // Fails if tray-icon ever changes its icon ID; the dialog keeps the error visible then.
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        show_error(message);
    }
}

/// Copies `text` into a fixed UTF-16 field, cut short so the closing NUL fits.
fn copy_wide(field: &mut [u16], text: &str) {
    let room = field.len() - 1;
    for (unit, value) in field[..room].iter_mut().zip(text.encode_utf16()) {
        *unit = value;
    }
}

/// What keydock cannot work without, for the startup toast.
fn startup_error(keyboards: &Result<Vec<Keyboard>, ScanError>, found: &Found) -> Option<String> {
    match keyboards {
        Err(error @ ScanError::DriverUnavailable) => Some(format!(
            "{error}. To install it, run install-interception.exe /install as administrator in \
             keydock's folder, then restart Windows."
        )),
        Err(error) => Some(error.to_string()),
        Ok(_) if found.built_in == 0 => Some(
            "No blockable built-in keyboard found, so keydock stays inactive. The Keyboards menu \
             lists what it found."
                .into(),
        ),
        Ok(_) => None,
    }
}

/// Takes the single-instance lock; `None` means another keydock already holds it.
fn single_instance_lock() -> windows::core::Result<Option<HANDLE>> {
    match unsafe { CreateMutexW(None, false, w!(r"Local\keydock")) } {
        Ok(_) if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS => Ok(None),
        Ok(handle) => Ok(Some(handle)),
        // An elevated keydock's lock denies access to a normal start.
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => Ok(None),
        Err(error) => Err(error),
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    // The handle stays open until the process exits; that is what holds the single-instance lock.
    let Some(_instance) = single_instance_lock()? else {
        return Ok(());
    };
    let (settings, config_error) = load_settings();
    let mut notices: Vec<String> = config_error.into_iter().collect();
    let main_thread = unsafe { GetCurrentThreadId() };
    // Started before the first scan, so a keyboard plugged in meanwhile still causes a rescan.
    let (change_sender, changes) = mpsc::channel();
    let watch = KeyboardWatch::start(move |change| {
        // Fails only once the message loop has ended.
        let _ = change_sender.send(change);
        wake(main_thread);
    })
    .map_err(|error| {
        notices.push(format!(
            "Keyboard changes are not watched ({error}); restart keydock after you plug a \
             keyboard in or out"
        ));
    })
    .ok();
    let (keyboards, driver) = match Driver::open() {
        Ok(driver) => (Ok(driver.keyboards()), Some(driver)),
        Err(error) => (Err(error), None),
    };
    let (menu, found, items) = build_menu(&notices, &keyboards, &settings.config)?;
    let (tray, menu_events) = build_tray(&menu)?;
    if let Some(message) = startup_error(&keyboards, &found) {
        notify_error(&tray, &message);
    }
    // Spawned before the first refresh: the thread clears any filter left on the built-in
    // keyboard before it reads the first device set.
    let blocker =
        driver.map(|driver| Blocker::spawn(driver, found.built_in, move || wake(main_thread)));
    let mut app = App {
        policy: Policy {
            paused: settings.config.paused,
            ..Policy::default()
        },
        menu,
        tray,
        update: MenuItem::new("", true, None),
        update_shown: false,
        update_note: None,
        pause: items.pause,
        reenable: items.reenable,
        login: items.login,
        quit_id: items.quit.id().clone(),
        icon_blocking: Cell::new(false),
        tray_stale: Cell::new(false),
        keyboards_menu: items.keyboards,
        changes,
        rescans: Rescans::default(),
        _watch: watch,
        found,
        settings,
        blocker,
    };
    app.policy.set_triggered(app.triggered());
    // The driver's list may still lack a keyboard plugged in just before startup.
    app.rescans.start();
    app.refresh();

    let (results, update_results) = mpsc::channel();
    update::spawn_checker(results, move || wake(main_thread));
    app.message_loop(&update_results, &menu_events)
}

/// Wakes the message loop. If the post is lost, the next message does the same job.
fn wake(main_thread: u32) {
    let _ = unsafe { PostThreadMessageW(main_thread, WM_APP, WPARAM(0), LPARAM(0)) };
}

struct MenuItems {
    /// `None` when the driver is not available, so there is no list to update.
    keyboards: Option<Submenu>,
    pause: CheckMenuItem,
    reenable: MenuItem,
    login: CheckMenuItem,
    quit: MenuItem,
}

fn build_menu(
    notices: &[String],
    keyboards: &Result<Vec<Keyboard>, ScanError>,
    config: &Config,
) -> Result<(Menu, Found, MenuItems), Box<dyn Error>> {
    let menu = Menu::new();
    for notice in notices {
        menu.append(&disabled(notice))?;
    }
    let (found, submenu) = match keyboards {
        Ok(keyboards) => {
            let submenu = Submenu::new("Keyboards", true);
            let found = fill_keyboards(&submenu, keyboards, &devices::present_keyboards(), config)?;
            menu.append(&submenu)?;
            (found, Some(submenu))
        }
        Err(error) => {
            menu.append(&disabled(&format!("Keyboards: {error}")))?;
            (Found::default(), None)
        }
    };
    let login = match autostart::enabled() {
        Ok(login) => login,
        Err(error) => {
            menu.append(&disabled(&format!("Start at login state unknown: {error}")))?;
            false
        }
    };
    let items = MenuItems {
        keyboards: submenu,
        pause: CheckMenuItem::new("Pause keydock", true, config.paused, None),
        // Always enabled: muda disables items without graying them, so a disabled item looks clickable.
        reenable: MenuItem::new("Re-enable until next connect", true, None),
        login: CheckMenuItem::new("Start at login", true, login, None),
        quit: MenuItem::new("Quit", true, None),
    };
    menu.append_items(&[
        &PredefinedMenuItem::separator(),
        &items.pause,
        &items.reenable,
        &PredefinedMenuItem::separator(),
        &items.login,
        &PredefinedMenuItem::separator(),
        &items.quit,
    ])?;
    Ok((menu, found, items))
}

/// Builds the tray icon; the receiver gets the ID of each clicked menu item.
fn build_tray(menu: &Menu) -> Result<(TrayIcon, mpsc::Receiver<MenuId>), Box<dyn Error>> {
    let (clicks, menu_events) = mpsc::channel();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        // The native menu dispatches its command on the message-loop thread, which drains this
        // channel right after. Sending fails only once the loop has ended.
        let _ = clicks.send(event.id);
    }));
    // Without a handler, tray-icon queues pointer events that keydock never reads.
    tray_icon::TrayIconEvent::set_event_handler(Some(|_| {}));
    let tray = TrayIconBuilder::new()
        .with_tooltip(tooltip(Status::Inactive, None))
        .with_icon(keyboard_icon(false)?)
        .with_menu(Box::new(menu.clone()))
        .build()?;
    Ok((tray, menu_events))
}

struct Settings {
    config: Config,
    /// `None` when saving would overwrite a config keydock could not read, or the folder is unknown.
    path: Option<PathBuf>,
}

impl Settings {
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(error) = config::save(path, &self.config) {
            show_error(&format!(
                "keydock could not save its settings: {error}\nThe change applies until keydock \
                 quits; the next change saves all settings again."
            ));
        }
    }
}

/// Returns the settings and, if the config could not be used, a message for the menu.
fn load_settings() -> (Settings, Option<String>) {
    let unsaved = |error: String| {
        let settings = Settings {
            config: Config::default(),
            path: None,
        };
        (settings, Some(format!("{error}; changes are not saved")))
    };
    let path = match config::path() {
        Ok(path) => path,
        Err(error) => return unsaved(format!("Config folder unknown: {}", error.message())),
    };
    match config::load(&path) {
        Ok(config) => (
            Settings {
                config,
                path: Some(path),
            },
            None,
        ),
        Err(error) => unsaved(error),
    }
}

/// What the last keyboard scan found.
#[derive(Default)]
struct Found {
    /// What the driver and Windows listed, to tell whether a new scan changed anything.
    keyboards: Vec<Keyboard>,
    present: Vec<Present>,
    /// Each allowlist checkbox and its VID:PID.
    checkboxes: HashMap<MenuId, String>,
    /// VID:PID of each connected keyboard that has a driver slot, so it can trigger the block.
    external: Vec<String>,
    built_in: Devices,
}

fn disabled(text: &str) -> MenuItem {
    MenuItem::new(interception::menu_text(text), false, None)
}

/// Lists the keyboards in `submenu`: built-in ones disabled, one checkbox per external VID:PID.
fn fill_keyboards(
    submenu: &Submenu,
    keyboards: &[Keyboard],
    present: &windows::core::Result<Vec<Present>>,
    config: &Config,
) -> Result<Found, Box<dyn Error>> {
    let mut found = Found {
        keyboards: keyboards.to_vec(),
        present: present.as_ref().map_or_else(|_| Vec::new(), Clone::clone),
        ..Found::default()
    };
    if let Err(error) = present {
        // Without Windows' list keydock knows neither the built-in nor the external keyboards.
        let text = format!("Keyboard check failed: {}", error.message());
        submenu.append(&disabled(&text))?;
    }
    for entry in entries(keyboards, &found.present) {
        match entry {
            Entry::BuiltIn { device, id } => {
                found.built_in |= interception::device_bit(device);
                submenu.append(&disabled(&format!("{id} (built-in)")))?;
            }
            Entry::Unblockable(id) => {
                let text = format!("{id} (built-in, not blockable until Windows restarts)");
                submenu.append(&disabled(&text))?;
            }
            Entry::External { vid_pid, id, slot } => {
                let text = interception::menu_text(&format!("{vid_pid}: {id}"));
                let checked = config.allowlist.contains(&vid_pid);
                let item = CheckMenuItem::new(text, true, checked, None);
                submenu.append(&item)?;
                found.checkboxes.insert(item.id().clone(), vid_pid.clone());
                if slot {
                    found.external.push(vid_pid);
                } else {
                    // Right after a plug-in this is often only the driver's lag.
                    let text = format!(
                        "{vid_pid}: no Interception slot yet; if this stays, restart Windows"
                    );
                    submenu.append(&disabled(&text))?;
                }
            }
            Entry::Other(id) => submenu.append(&disabled(&id))?,
        }
    }
    if found.built_in == 0 {
        submenu.append(&disabled("No blockable built-in keyboard found"))?;
    }
    Ok(found)
}

#[derive(Debug, PartialEq)]
enum Entry {
    /// Part of the PC and in the driver's list, so keydock can block it.
    BuiltIn {
        device: i32,
        id: String,
    },
    /// Part of the PC but missing from the driver's list, so keydock cannot block it.
    Unblockable(String),
    External {
        vid_pid: String,
        id: String,
        /// Every interface has a driver slot. Without one the driver drops the keyboard's input,
        /// so it must not trigger the block.
        slot: bool,
    },
    Other(String),
}

/// The keyboard list. Windows' list decides what is present: the driver stops listing keyboards
/// after about 10 arrivals since boot. The driver's list only supplies the device to block.
fn entries(driver: &[Keyboard], present: &[Present]) -> Vec<Entry> {
    let in_driver = |id: &str| driver.iter().find(|k| k.id.eq_ignore_ascii_case(id));
    let mut entries = Vec::new();
    let mut models: Vec<String> = Vec::new();
    for keyboard in present {
        if keyboard.internal {
            let slot = in_driver(&keyboard.id);
            entries.push(match slot {
                Some(slot) => Entry::BuiltIn {
                    device: slot.device,
                    id: slot.id.clone(),
                },
                None => Entry::Unblockable(keyboard.id.clone()),
            });
            continue;
        }
        match interception::vid_pid(&keyboard.id) {
            // A keyboard with several interfaces lists the same VID:PID more than once.
            Some(vid_pid) if models.contains(&vid_pid) => {}
            Some(vid_pid) => {
                models.push(vid_pid.clone());
                let slot = present
                    .iter()
                    .filter(|p| interception::vid_pid(&p.id).as_ref() == Some(&vid_pid))
                    .all(|p| in_driver(&p.id).is_some());
                entries.push(Entry::External {
                    vid_pid,
                    id: keyboard.id.clone(),
                    slot,
                });
            }
            None => entries.push(Entry::Other(keyboard.id.clone())),
        }
    }
    entries
}

/// Scans again for `RESCAN_WINDOW` after each keyboard change, on a thread timer.
#[derive(Default)]
struct Rescans {
    /// 0 when no timer runs.
    timer: usize,
    until: Option<Instant>,
}

impl Rescans {
    fn start(&mut self) {
        self.until = Some(Instant::now() + RESCAN_WINDOW);
        if self.timer == 0 {
            // If this fails, only the scan right at each report happens.
            self.timer = unsafe { SetTimer(None, 0, RESCAN_INTERVAL_MS, None) };
        }
    }

    /// Whether to scan now. After the window it stops the timer and allows one last scan.
    fn due(&mut self) -> bool {
        match self.until {
            None => false,
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                self.until = None;
                if self.timer != 0 {
                    // A timer that fails to stop only causes extra ticks, which `until` ignores.
                    let _ = unsafe { KillTimer(None, self.timer) };
                    self.timer = 0;
                }
                true
            }
        }
    }
}

/// The user's choices that decide whether to block.
#[derive(Default)]
struct Policy {
    paused: bool,
    /// "Re-enable until next connect" was chosen.
    suspended: bool,
    /// An allowlisted keyboard is connected.
    triggered: bool,
}

impl Policy {
    /// The arrival of an allowlisted keyboard ends a suspension.
    fn set_triggered(&mut self, triggered: bool) {
        if triggered && !self.triggered {
            self.suspended = false;
        }
        self.triggered = triggered;
    }

    fn blocking(&self) -> bool {
        self.triggered && !self.paused && !self.suspended
    }

    /// Windows reported an allowlisted keyboard arriving. Scans lag that report, so a quick
    /// unplug and replug may never show the keyboard missing; the arrival alone ends a suspension.
    fn allowlisted_arrived(&mut self) {
        self.suspended = false;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Inactive,
    Blocking,
    Paused,
    Suspended,
}

fn tooltip(status: Status, update_note: Option<&str>) -> String {
    let text = match status {
        Status::Inactive => "keydock: inactive",
        Status::Blocking => "keydock: blocking the built-in keyboard",
        Status::Paused => "keydock: paused",
        Status::Suspended => "keydock: built-in keyboard on until the next connect",
    };
    match update_note {
        Some(note) => format!("{text} ({note})"),
        None => text.to_string(),
    }
}

struct App {
    menu: Menu,
    tray: TrayIcon,
    update: MenuItem,
    update_shown: bool,
    update_note: Option<String>,
    pause: CheckMenuItem,
    reenable: MenuItem,
    login: CheckMenuItem,
    quit_id: MenuId,
    /// Whether the tray shows the blocked icon.
    icon_blocking: Cell<bool>,
    /// The last tray update failed, so the tray may show an old state.
    tray_stale: Cell<bool>,
    keyboards_menu: Option<Submenu>,
    changes: mpsc::Receiver<Change>,
    rescans: Rescans,
    _watch: Option<KeyboardWatch>,
    found: Found,
    settings: Settings,
    policy: Policy,
    // Dropped last of the state that matters: its drop clears the filter and stops the thread.
    blocker: Option<Blocker>,
}

impl App {
    fn triggered(&self) -> bool {
        let allowlist = &self.settings.config.allowlist;
        self.found
            .external
            .iter()
            .any(|vid_pid| allowlist.contains(vid_pid))
    }

    fn status(&self) -> Status {
        let can_block = self.blocker.is_some() && self.found.built_in != 0;
        if self.policy.paused {
            Status::Paused
        } else if can_block && self.policy.blocking() {
            Status::Blocking
        } else if self.policy.suspended && self.policy.triggered {
            Status::Suspended
        } else {
            Status::Inactive
        }
    }

    /// Applies the current state: the block first, then the icon and tooltip.
    fn refresh(&self) {
        let status = self.status();
        let blocking = status == Status::Blocking;
        if let Some(blocker) = &self.blocker {
            blocker.block(if blocking { self.found.built_in } else { 0 });
        }
        // The shell refuses tray updates while Explorer restarts or hangs. The tray only shows
        // the state, so that must not stop keydock; the message loop retries on its next message.
        self.tray_stale
            .set(self.show_tray(status, blocking).is_err());
    }

    fn show_tray(&self, status: Status, blocking: bool) -> Result<(), Box<dyn Error>> {
        // Rescans refresh every 250 ms; only a change needs a new icon.
        if blocking != self.icon_blocking.get() {
            self.tray.set_icon(Some(keyboard_icon(blocking)?))?;
            self.icon_blocking.set(blocking);
        }
        self.tray
            .set_tooltip(Some(tooltip(status, self.update_note.as_deref())))?;
        Ok(())
    }

    /// Returns `false` when the user chose Quit.
    fn on_click(&mut self, id: &MenuId) -> bool {
        if *id == self.quit_id {
            return false;
        }
        if id == self.update.id() {
            open_releases_page();
        } else if id == self.pause.id() {
            // muda has already toggled the check mark.
            self.policy.paused = self.pause.is_checked();
            self.settings.config.paused = self.policy.paused;
            self.settings.save();
        } else if id == self.reenable.id() {
            self.policy.suspended = true;
        } else if id == self.login.id() {
            // muda has already toggled the check mark; undo it if the registry refuses.
            let on = self.login.is_checked();
            if let Err(error) = autostart::set(on) {
                self.login.set_checked(!on);
                show_error(&format!("keydock could not change Start at login: {error}"));
            }
        } else if let Some(vid_pid) = self.found.checkboxes.get(id) {
            let allowlist = &mut self.settings.config.allowlist;
            if !allowlist.remove(vid_pid) {
                allowlist.insert(vid_pid.clone());
            }
            self.settings.save();
            self.policy.set_triggered(self.triggered());
        }
        self.refresh();
        true
    }

    fn message_loop(
        &mut self,
        update_results: &mpsc::Receiver<Outcome>,
        clicks: &mpsc::Receiver<MenuId>,
    ) -> Result<(), Box<dyn Error>> {
        let mut message = MSG::default();
        loop {
            match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
                -1 => return Err(io::Error::last_os_error().into()),
                0 => return Ok(()),
                _ => unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                },
            }
            // Drained on every message: a thread message posted while the menu is open never arrives here.
            for result in update_results.try_iter() {
                // The update notice is not worth stopping keydock for.
                if let Err(error) = self.show_update(result) {
                    show_error(&format!("keydock could not show the update check: {error}"));
                }
            }
            for id in clicks.try_iter() {
                if !self.on_click(&id) {
                    return Ok(());
                }
            }
            let mut changed = false;
            for change in self.changes.try_iter() {
                changed = true;
                let allowlist = &self.settings.config.allowlist;
                if change.arrived
                    && interception::vid_pid(&change.link)
                        .is_some_and(|vid_pid| allowlist.contains(&vid_pid))
                {
                    self.policy.allowlisted_arrived();
                }
            }
            if changed {
                self.rescans.start();
            }
            let tick = message.message == WM_TIMER && message.wParam.0 == self.rescans.timer;
            if (changed || tick)
                && self.rescans.due()
                && let Err(error) = self.rescan()
            {
                // Without a reliable list keydock cannot know what is connected, so it stops
                // blocking; the next keyboard change tries again. The policy and checkboxes stay,
                // so the next good scan is not taken for a new connection.
                self.found.built_in = 0;
                // Forces the next scan to rebuild the menu.
                self.found.keyboards.clear();
                self.found.present.clear();
                self.refresh();
                show_error(&format!(
                    "keydock could not update the keyboard list: {error}"
                ));
            }
            if self.blocker.as_ref().is_some_and(Blocker::stopped) {
                // Dropping joins the finished thread. Its driver connection closed, which
                // cleared the filters, so the built-in keyboard already works.
                self.blocker = None;
                show_error(
                    "keydock's input thread stopped, so the built-in keyboard is no longer \
                     blocked. Restart keydock to block it again.",
                );
                self.refresh();
            }
            if self.tray_stale.get() {
                self.refresh();
            }
        }
    }

    /// Reads the connected keyboards again and applies the result.
    fn rescan(&mut self) -> Result<(), Box<dyn Error>> {
        let (Some(blocker), Some(submenu)) = (&self.blocker, &self.keyboards_menu) else {
            return Ok(());
        };
        // A stopped thread has its own message in the loop.
        if blocker.stopped() {
            return Ok(());
        }
        let Some(keyboards) = blocker.keyboards() else {
            return Err("the input thread did not answer".into());
        };
        // A failed list is a failed scan, not "nothing connected": that would look like an unplug,
        // and the next good scan would end Re-enable.
        let present = devices::present_keyboards()?;
        let unchanged = keyboards == self.found.keyboards && present == self.found.present;
        // Most scans in a rescan window find nothing new; rebuilding the menu then is wasted.
        if !unchanged {
            while submenu.remove_at(0).is_some() {}
            let found = fill_keyboards(submenu, &keyboards, &Ok(present), &self.settings.config)?;
            self.found = found;
            self.policy.set_triggered(self.triggered());
        }
        // Also applies an arrival that ended a suspension.
        self.refresh();
        Ok(())
    }

    /// Puts the result of an update check in the menu and tooltip.
    fn show_update(&mut self, result: Outcome) -> Result<(), Box<dyn Error>> {
        match result {
            Outcome::UpToDate => {
                self.update_note = None;
                if self.update_shown {
                    self.menu.remove(&self.update)?;
                    self.update_shown = false;
                }
            }
            Outcome::Available(tag) => {
                self.update_note = Some(format!("update {tag} available"));
                self.show_update_item(format!("Update available: {tag}"))?;
            }
            Outcome::Failed(reason) => {
                self.update_note = None;
                // Clickable like the available notice: muda cannot gray an item it disables.
                self.show_update_item(format!("Update check failed: {reason}"))?;
            }
        }
        self.refresh();
        Ok(())
    }

    /// The item opens the release page when clicked.
    fn show_update_item(&mut self, text: String) -> Result<(), Box<dyn Error>> {
        self.update.set_text(text);
        if !self.update_shown {
            self.menu.insert(&self.update, 0)?;
            self.update_shown = true;
        }
        Ok(())
    }
}

fn open_releases_page() {
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(update::RELEASES_PAGE),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= SHELL_EXECUTE_MAX_ERROR {
        show_error(&format!("keydock could not open {}", update::RELEASES_PAGE));
    }
}

/// The red slash marks the built-in keyboard as blocked.
fn keyboard_icon(blocking: bool) -> Result<Icon, tray_icon::BadIcon> {
    // Square, because the shell stretches tray icons to a square slot.
    const BLOCKED: [&str; 16] = [
        "                ",
        "             rr ",
        "            rr  ",
        "           rr   ",
        " #########rr### ",
        " #.......rr...# ",
        " #.#.#.#rr.##.# ",
        " #.....rr.....# ",
        " #.##.rr#.#.#.# ",
        " #...rr.......# ",
        " #..rr#####...# ",
        " ##rr########## ",
        "  rr            ",
        " rr             ",
        "rr              ",
        "                ",
    ];
    const KEYS: [&str; 16] = [
        "                ",
        "                ",
        "                ",
        "                ",
        " ############## ",
        " #............# ",
        " #.#.#.#.#.##.# ",
        " #............# ",
        " #.##.#.#.#.#.# ",
        " #............# ",
        " #...######...# ",
        " ############## ",
        "                ",
        "                ",
        "                ",
        "                ",
    ];
    let rows = if blocking { BLOCKED } else { KEYS };
    let rgba = rows
        .iter()
        .flat_map(|row| row.bytes())
        .flat_map(|pixel| match pixel {
            b'#' => [240, 240, 240, 255],
            b'.' => [55, 55, 55, 255],
            b'r' => [225, 45, 45, 255],
            _ => [0, 0, 0, 0],
        });
    Icon::from_rgba(rgba.collect(), rows[0].len() as u32, rows.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard(device: i32, id: &str) -> Keyboard {
        Keyboard {
            device,
            id: id.to_string(),
        }
    }

    fn present(id: &str, internal: bool) -> Present {
        Present {
            id: id.to_string(),
            internal,
        }
    }

    const ITE: &str = r"HID\VID_048D&PID_C197&REV_1800&Col04";
    const KEYCHRON: &str = r"HID\VID_3434&PID_0351&REV_0101&MI_00";

    const KEYCHRON_2: &str = r"HID\VID_3434&PID_0351&REV_0101&MI_02&Col04";

    #[test]
    fn a_keyboard_missing_from_the_driver_is_listed_without_a_slot() {
        let driver = [keyboard(2, ITE)];
        let present = [present(ITE, true), present(KEYCHRON, false)];
        assert_eq!(
            entries(&driver, &present),
            [
                Entry::BuiltIn {
                    device: 2,
                    id: ITE.into()
                },
                Entry::External {
                    vid_pid: "3434:0351".into(),
                    id: KEYCHRON.into(),
                    slot: false
                },
            ]
        );
    }

    #[test]
    fn a_keyboard_has_a_slot_only_if_every_interface_has_one() {
        let present = [present(KEYCHRON, false), present(KEYCHRON_2, false)];
        let external = |slot| Entry::External {
            vid_pid: "3434:0351".into(),
            id: KEYCHRON.into(),
            slot,
        };
        let both = [
            keyboard(3, KEYCHRON),
            keyboard(4, &KEYCHRON_2.to_lowercase()),
        ];
        assert_eq!(entries(&both, &present), [external(true)]);
        assert_eq!(entries(&both[1..], &present), [external(false)]);
    }

    #[test]
    fn a_built_in_keyboard_needs_a_driver_slot_to_be_blockable() {
        let present = [present(ITE, true)];
        assert_eq!(entries(&[], &present), [Entry::Unblockable(ITE.into())]);
    }

    #[test]
    fn windows_decides_what_is_present_and_one_model_gets_one_entry() {
        // The driver still lists a keyboard that Windows already reported gone.
        let driver = [keyboard(3, KEYCHRON)];
        let present = [
            present(r"HID\VID_046D&PID_C547&REV_0402&MI_01&Col01", false),
            present(r"HID\VID_046D&PID_C547&REV_0402&MI_02", false),
            present(r"ACPI\VEN_ABCD&DEV_0001", false),
        ];
        assert_eq!(
            entries(&driver, &present),
            [
                Entry::External {
                    vid_pid: "046D:C547".into(),
                    id: r"HID\VID_046D&PID_C547&REV_0402&MI_01&Col01".into(),
                    slot: false
                },
                Entry::Other(r"ACPI\VEN_ABCD&DEV_0001".into()),
            ]
        );
    }

    #[test]
    fn blocks_only_while_triggered_and_neither_paused_nor_suspended() {
        let mut policy = Policy::default();
        assert!(!policy.blocking());
        policy.set_triggered(true);
        assert!(policy.blocking());
        policy.paused = true;
        assert!(!policy.blocking());
        policy.paused = false;
        policy.suspended = true;
        assert!(!policy.blocking());
    }

    #[test]
    fn a_suspension_lasts_until_an_allowlisted_keyboard_arrives_again() {
        let mut policy = Policy::default();
        policy.set_triggered(true);
        policy.suspended = true;
        policy.set_triggered(true);
        assert!(policy.suspended, "still connected is not a new arrival");
        policy.set_triggered(false);
        assert!(policy.suspended);
        policy.set_triggered(true);
        assert!(!policy.suspended);
        assert!(policy.blocking());
    }

    #[test]
    fn an_arrival_report_ends_a_suspension_even_if_no_scan_saw_the_keyboard_leave() {
        let mut policy = Policy::default();
        policy.set_triggered(true);
        policy.suspended = true;
        // The scans after the unplug and the replug both still list the keyboard.
        policy.set_triggered(true);
        policy.allowlisted_arrived();
        policy.set_triggered(true);
        assert!(policy.blocking());
    }

    #[test]
    fn a_pause_survives_an_arrival() {
        let mut policy = Policy {
            paused: true,
            ..Policy::default()
        };
        policy.set_triggered(true);
        assert!(!policy.blocking());
    }

    #[test]
    fn copy_wide_keeps_the_closing_nul_when_it_cuts_text() {
        // NOTIFYICONDATAW starts zeroed, so the last unit stays the NUL.
        let mut field = [0u16; 4];
        copy_wide(&mut field, "abcdef");
        assert_eq!(field, [b'a'.into(), b'b'.into(), b'c'.into(), 0]);
    }

    #[test]
    fn startup_toasts_only_for_a_missing_driver_or_built_in_keyboard() {
        let blockable = Found {
            built_in: interception::device_bit(2),
            ..Found::default()
        };
        let driver = startup_error(&Err(ScanError::DriverUnavailable), &blockable).unwrap();
        assert!(driver.contains("install-interception.exe"), "{driver}");
        let none_found = startup_error(&Ok(Vec::new()), &Found::default()).unwrap();
        assert!(
            none_found.contains("No blockable built-in keyboard"),
            "{none_found}"
        );
        assert_eq!(startup_error(&Ok(Vec::new()), &blockable), None);
    }

    #[test]
    fn both_tray_icons_build() {
        // The blocked icon is first built while blocking, so a bad pixel map would fail only then.
        assert!(keyboard_icon(false).is_ok());
        assert!(keyboard_icon(true).is_ok());
    }

    #[test]
    fn tooltip_adds_the_update_note() {
        assert_eq!(
            tooltip(Status::Blocking, Some("update v0.2.0 available")),
            "keydock: blocking the built-in keyboard (update v0.2.0 available)"
        );
        assert_eq!(tooltip(Status::Paused, None), "keydock: paused");
    }
}
