#![windows_subsystem = "windows"]

use std::{error::Error, io, process::ExitCode};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};
use windows::{
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MB_ICONERROR, MSG, MessageBoxW, PostQuitMessage,
        TranslateMessage,
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
    const KEYS: [&str; 8] = [
        "##############",
        "#............#",
        "#.#.#.#.#.##.#",
        "#............#",
        "#.##.#.#.#.#.#",
        "#............#",
        "#...######...#",
        "##############",
    ];
    let rgba = KEYS.iter().flat_map(|row| row.bytes()).flat_map(|pixel| {
        if pixel == b'#' {
            [240, 240, 240, 255]
        } else {
            [55, 55, 55, 255]
        }
    });
    Icon::from_rgba(rgba.collect(), KEYS[0].len() as u32, KEYS.len() as u32)
}
