#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("keydock supports Windows only");

mod update;

use std::{error::Error, io, process::ExitCode, sync::mpsc};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
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
                DispatchMessageW, GetMessageW, MB_ICONERROR, MSG, MessageBoxW, PostQuitMessage,
                PostThreadMessageW, SW_SHOWNORMAL, TranslateMessage, WM_APP,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

const TOOLTIP: &str = "keydock: inactive";
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
    let quit = MenuItem::new("Quit", true, None);
    let update = MenuItem::new("", true, None);
    let menu = Menu::with_items(&[&quit])?;
    let quit_id = quit.id().clone();
    let update_id = update.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        // The native menu dispatches its command on the tray's message-loop thread.
        if event.id == quit_id {
            unsafe { PostQuitMessage(0) };
        } else if event.id == update_id {
            open_releases_page();
        }
    }));
    // Without a handler, tray-icon queues pointer events that keydock never reads.
    tray_icon::TrayIconEvent::set_event_handler(Some(|_| {}));
    let tray = TrayIconBuilder::new()
        .with_tooltip(TOOLTIP)
        .with_icon(keyboard_icon()?)
        .with_menu(Box::new(menu.clone()))
        .build()?;

    let (results, update_results) = mpsc::channel();
    let main_thread = unsafe { GetCurrentThreadId() };
    update::spawn_checker(results, move || {
        // Only wakes the message loop; if the post is lost, the next message drains the result.
        let _ = unsafe { PostThreadMessageW(main_thread, WM_APP, WPARAM(0), LPARAM(0)) };
    });
    message_loop(&menu, &update, &tray, &update_results)
}

fn message_loop(
    menu: &Menu,
    update: &MenuItem,
    tray: &TrayIcon,
    update_results: &mpsc::Receiver<Outcome>,
) -> Result<(), Box<dyn Error>> {
    let mut update_shown = false;
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
            match show_update(menu, update, tray, update_shown, result) {
                Ok(shown) => update_shown = shown,
                // The update notice is not worth stopping keydock for.
                Err(error) => {
                    show_error(&format!("keydock could not show the update check: {error}"))
                }
            }
        }
    }
}

/// Puts the result of an update check in the menu and tooltip; returns whether the item is shown.
/// The menu change comes last, so an error leaves `shown` true to the menu.
fn show_update(
    menu: &Menu,
    item: &MenuItem,
    tray: &TrayIcon,
    shown: bool,
    result: Outcome,
) -> Result<bool, Box<dyn Error>> {
    let (text, enabled, tooltip) = match result {
        Outcome::UpToDate => {
            tray.set_tooltip(Some(TOOLTIP))?;
            if shown {
                menu.remove(item)?;
            }
            return Ok(false);
        }
        Outcome::Available(tag) => (
            format!("Update available: {tag}"),
            true,
            format!("{TOOLTIP} (update {tag} available)"),
        ),
        Outcome::Failed(reason) => (
            format!("Update check failed: {reason}"),
            false,
            TOOLTIP.to_string(),
        ),
    };
    tray.set_tooltip(Some(tooltip))?;
    item.set_text(text);
    item.set_enabled(enabled);
    if !shown {
        menu.insert(item, 0)?;
    }
    Ok(true)
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
