#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("keydock supports Windows only");

use std::{error::Error, io, process::ExitCode};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};
use windows::{
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, MB_ICONERROR, MSG, MessageBoxW, PostQuitMessage,
            TranslateMessage,
        },
    },
    core::{HSTRING, w},
};

fn main() -> ExitCode {
    if let Err(error) = run() {
        let message = HSTRING::from(format!("keydock could not continue: {error}"));
        unsafe { MessageBoxW(None, &message, w!("keydock"), MB_ICONERROR) };
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<(), Box<dyn Error>> {
    // The handle stays open until the process exits; that is what holds the single-instance lock.
    let instance = unsafe { CreateMutexW(None, false, w!(r"Local\keydock")) };
    let already_running = match &instance {
        Ok(_) => unsafe { GetLastError() == ERROR_ALREADY_EXISTS },
        // An elevated keydock's lock denies access to a normal start.
        Err(error) => error.code() == ERROR_ACCESS_DENIED.to_hresult(),
    };
    if already_running {
        return Ok(());
    }
    let _instance = instance?;
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::with_items(&[&quit])?;
    let quit_id = quit.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == quit_id {
            // The native menu dispatches its command on the tray's message-loop thread.
            unsafe { PostQuitMessage(0) };
        }
    }));
    // Without a handler, tray-icon queues pointer events that keydock never reads.
    tray_icon::TrayIconEvent::set_event_handler(Some(|_| {}));
    let _tray = TrayIconBuilder::new()
        .with_tooltip("keydock: inactive")
        .with_icon(keyboard_icon()?)
        .with_menu(Box::new(menu))
        .build()?;

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
