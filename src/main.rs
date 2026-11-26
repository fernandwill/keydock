#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("keydock supports Windows only");

mod config;
mod devices;
mod interception;
mod update;

use config::Config;
use interception::{Blocker, Devices, Driver, Keyboard, Kind, ScanError};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io,
    path::PathBuf,
    process::ExitCode,
    sync::mpsc,
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
};
use update::Outcome;
use windows::{
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM,
        },
        System::Threading::{CreateMutexW, GetCurrentThreadId},
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, MB_ICONERROR, MSG, MessageBoxW, PostThreadMessageW,
                SW_SHOWNORMAL, TranslateMessage, WM_APP,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

// ShellExecuteW reports success with a value above this.
const SHELL_EXECUTE_MAX_ERROR: isize = 32;

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
    let (keyboards, driver) = match Driver::open() {
        Ok(driver) => (Ok(driver.keyboards()), Some(driver)),
        Err(error) => (Err(error), None),
    };
    let menu = Menu::new();
    if let Some(error) = config_error {
        menu.append(&disabled(&error))?;
    }
    let found = append_keyboards(
        &menu,
        &keyboards,
        &devices::internal_keyboards(),
        &settings.config,
    )?;
    let pause = CheckMenuItem::new("Pause keydock", true, settings.config.paused, None);
    // Always enabled: muda disables items without graying them, so a disabled item looks clickable.
    let reenable = MenuItem::new("Re-enable until next connect", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[
        &PredefinedMenuItem::separator(),
        &pause,
        &reenable,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;
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
        .with_icon(keyboard_icon()?)
        .with_menu(Box::new(menu.clone()))
        .build()?;
    // Spawned before the first refresh: the thread clears any filter left on the built-in
    // keyboard before it reads the first device set.
    let blocker = driver.map(|driver| Blocker::spawn(driver, found.built_in));
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
        pause,
        reenable,
        quit_id: quit.id().clone(),
        found,
        settings,
        blocker,
    };
    app.policy.set_triggered(app.triggered());
    app.refresh()?;

    let (results, update_results) = mpsc::channel();
    let main_thread = unsafe { GetCurrentThreadId() };
    update::spawn_checker(results, move || {
        // Only wakes the message loop; if the post is lost, the next message drains the result.
        let _ = unsafe { PostThreadMessageW(main_thread, WM_APP, WPARAM(0), LPARAM(0)) };
    });
    app.message_loop(&update_results, &menu_events)
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

/// What the startup scan found.
#[derive(Default)]
struct Found {
    /// Each allowlist checkbox and its VID:PID.
    checkboxes: HashMap<MenuId, String>,
    /// VID:PID of each connected keyboard that can go on the allowlist.
    external: Vec<String>,
    built_in: Devices,
}

fn disabled(text: &str) -> MenuItem {
    MenuItem::new(interception::menu_text(text), false, None)
}

/// Lists the keyboards: built-in ones disabled, one checkbox per external VID:PID.
fn append_keyboards(
    menu: &Menu,
    keyboards: &Result<Vec<Keyboard>, ScanError>,
    internal: &windows::core::Result<HashSet<String>>,
    config: &Config,
) -> Result<Found, Box<dyn Error>> {
    let mut found = Found::default();
    let keyboards = match keyboards {
        Ok(keyboards) => keyboards,
        Err(error) => {
            menu.append(&disabled(&format!("Keyboards: {error}")))?;
            return Ok(found);
        }
    };
    let submenu = Submenu::new("Keyboards", true);
    let none = HashSet::new();
    // Without this check keydock cannot tell which keyboard is built in, so it blocks nothing.
    let internal = internal.as_ref().unwrap_or_else(|error| {
        let text = format!("Built-in keyboard check failed: {}", error.message());
        // A failed append only loses this notice; the list below still works.
        let _ = submenu.append(&disabled(&text));
        &none
    });
    for keyboard in keyboards {
        match interception::classify(&keyboard.id, internal) {
            Kind::BuiltIn => {
                found.built_in |= interception::device_bit(keyboard.device);
                submenu.append(&disabled(&format!("{} (built-in)", keyboard.id)))?;
            }
            // A keyboard with several interfaces lists the same VID:PID more than once.
            Kind::External(vid_pid) if found.external.contains(&vid_pid) => {}
            Kind::External(vid_pid) => {
                let text = interception::menu_text(&format!("{vid_pid}: {}", keyboard.id));
                let checked = config.allowlist.contains(&vid_pid);
                let item = CheckMenuItem::new(text, true, checked, None);
                submenu.append(&item)?;
                found.checkboxes.insert(item.id().clone(), vid_pid.clone());
                found.external.push(vid_pid);
            }
            Kind::Other => submenu.append(&disabled(&keyboard.id))?,
        }
    }
    if found.built_in == 0 {
        submenu.append(&disabled("No built-in keyboard found"))?;
    }
    menu.append(&submenu)?;
    Ok(found)
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
    quit_id: MenuId,
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

    /// Applies the current state: the block first, then the tooltip.
    fn refresh(&self) -> Result<(), Box<dyn Error>> {
        let status = self.status();
        if let Some(blocker) = &self.blocker {
            let devices = if status == Status::Blocking {
                self.found.built_in
            } else {
                0
            };
            blocker.block(devices);
        }
        self.tray
            .set_tooltip(Some(tooltip(status, self.update_note.as_deref())))?;
        Ok(())
    }

    /// Returns `false` when the user chose Quit.
    fn on_click(&mut self, id: &MenuId) -> Result<bool, Box<dyn Error>> {
        if *id == self.quit_id {
            return Ok(false);
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
        } else if let Some(vid_pid) = self.found.checkboxes.get(id) {
            let allowlist = &mut self.settings.config.allowlist;
            if !allowlist.remove(vid_pid) {
                allowlist.insert(vid_pid.clone());
            }
            self.settings.save();
            self.policy.set_triggered(self.triggered());
        }
        self.refresh()?;
        Ok(true)
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
                match self.on_click(&id) {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    // refresh applies the block before it can fail, so keydock keeps running.
                    Err(error) => {
                        show_error(&format!("keydock could not apply the change: {error}"))
                    }
                }
            }
        }
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
                self.show_update_item(format!("Update available: {tag}"), true)?;
            }
            Outcome::Failed(reason) => {
                self.update_note = None;
                self.show_update_item(format!("Update check failed: {reason}"), false)?;
            }
        }
        self.refresh()
    }

    fn show_update_item(&mut self, text: String, enabled: bool) -> Result<(), Box<dyn Error>> {
        self.update.set_text(text);
        self.update.set_enabled(enabled);
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

fn keyboard_icon() -> Result<Icon, tray_icon::BadIcon> {
    // Square, because the shell stretches tray icons to a square slot.
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
    let rgba = KEYS
        .iter()
        .flat_map(|row| row.bytes())
        .flat_map(|pixel| match pixel {
            b'#' => [240, 240, 240, 255],
            b'.' => [55, 55, 55, 255],
            _ => [0, 0, 0, 0],
        });
    Icon::from_rgba(rgba.collect(), KEYS[0].len() as u32, KEYS.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_pause_survives_an_arrival() {
        let mut policy = Policy {
            paused: true,
            ..Policy::default()
        };
        policy.set_triggered(true);
        assert!(!policy.blocking());
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
