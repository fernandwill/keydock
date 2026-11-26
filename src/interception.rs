use std::{ffi::c_void, fmt};
use windows::{
    Win32::{
        Foundation::{ERROR_MOD_NOT_FOUND, FreeLibrary, HMODULE},
        System::LibraryLoader::{
            GetProcAddress, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LoadLibraryExW,
        },
    },
    core::{Error, PCSTR, w},
};

// Interception numbers keyboards 1 to 10 (INTERCEPTION_KEYBOARD(0..10)).
const KEYBOARD_DEVICES: std::ops::RangeInclusive<i32> = 1..=10;
// Interception's own samples use 500 wide characters for a hardware ID.
const HARDWARE_ID_UNITS: usize = 512;

type Proc = unsafe extern "system" fn() -> isize;
type CreateContext = unsafe extern "C" fn() -> *mut c_void;
type DestroyContext = unsafe extern "C" fn(*mut c_void);
type GetHardwareId = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, u32) -> u32;

pub enum ScanError {
    Load(Error),
    MissingFunction(&'static str),
    DriverUnavailable,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Load(error) if error.code() == ERROR_MOD_NOT_FOUND.to_hresult() => {
                write!(f, "interception.dll not found next to keydock.exe")
            }
            Self::Load(error) => write!(f, "interception.dll could not load: {}", error.message()),
            Self::MissingFunction(name) => write!(f, "interception.dll has no {name}"),
            Self::DriverUnavailable => write!(
                f,
                "Interception driver not available (not installed, or Windows not restarted after the install)"
            ),
        }
    }
}

struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        // Nothing useful can happen if unloading fails; keydock no longer calls into the DLL.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// Closed when dropped, also on a panic, so its 20 device handles and events never leak.
struct Context {
    raw: *mut c_void,
    destroy: DestroyContext,
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.raw) };
    }
}

impl Library {
    /// `name` must end with a NUL.
    fn function(&self, name: &'static str) -> Result<Proc, ScanError> {
        unsafe { GetProcAddress(self.0, PCSTR(name.as_ptr())) }
            .ok_or(ScanError::MissingFunction(name.trim_end_matches('\0')))
    }
}

/// Lists the first hardware ID of every connected keyboard. Sets no filter, so input is unchanged.
pub fn scan() -> Result<Vec<String>, ScanError> {
    // Only keydock's own folder: the current folder and PATH could hold a planted DLL.
    let library = Library(
        unsafe {
            LoadLibraryExW(
                w!("interception.dll"),
                None,
                LOAD_LIBRARY_SEARCH_APPLICATION_DIR,
            )
        }
        .map_err(ScanError::Load)?,
    );
    // Safe: these are the Interception 1.0.1 signatures (x64 has a single calling convention).
    // The DLL exports no version, and the LGPL lets users swap the file, so a hash pin is not allowed.
    let (create, destroy, get_hardware_id) = unsafe {
        (
            std::mem::transmute::<Proc, CreateContext>(
                library.function("interception_create_context\0")?,
            ),
            std::mem::transmute::<Proc, DestroyContext>(
                library.function("interception_destroy_context\0")?,
            ),
            std::mem::transmute::<Proc, GetHardwareId>(
                library.function("interception_get_hardware_id\0")?,
            ),
        )
    };
    let raw = unsafe { create() };
    if raw.is_null() {
        return Err(ScanError::DriverUnavailable);
    }
    // Declared after `library`, so it closes before the DLL unloads.
    let context = Context { raw, destroy };
    let mut keyboards = Vec::new();
    let mut buffer = [0u16; HARDWARE_ID_UNITS];
    for device in KEYBOARD_DEVICES {
        let bytes = unsafe {
            get_hardware_id(
                context.raw,
                device,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
            )
        };
        // An empty slot or an ID longer than the buffer returns 0 bytes.
        if let Some(id) = hardware_ids(&buffer, bytes).into_iter().next() {
            keyboards.push(id);
        }
    }
    Ok(keyboards)
}

/// Turns a REG_MULTI_SZ buffer of `bytes` length into its strings.
fn hardware_ids(buffer: &[u16], bytes: u32) -> Vec<String> {
    let units = (bytes as usize / 2).min(buffer.len());
    buffer[..units]
        .split(|&unit| unit == 0)
        .filter(|id| !id.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Built-in keyboards are PS/2 (`ACPI\`) or I2C HID (`HID\VEN_`); USB and Bluetooth use `VID_`.
fn is_builtin(id: &str) -> bool {
    let id = id.to_ascii_uppercase();
    id.starts_with("ACPI\\") || id.starts_with("HID\\VEN_")
}

/// Index of the only built-in keyboard; `None` when there is none or more than one.
pub fn find_builtin(ids: &[String]) -> Option<usize> {
    let mut matches = ids.iter().enumerate().filter(|(_, id)| is_builtin(id));
    match (matches.next(), matches.next()) {
        (Some((index, _)), None) => Some(index),
        _ => None,
    }
}

/// `VID:PID` in upper case from USB (`VID_046D&PID_C52B`) and Bluetooth (`VID&0002046d_PID&b342`) IDs.
pub fn vid_pid(id: &str) -> Option<String> {
    let id = id.to_ascii_uppercase();
    let vid = hex_after(&id, ["VID_", "VID&"])?;
    let pid = hex_after(&id, ["PID_", "PID&"])?;
    if vid.len() < 4 || pid.len() < 4 {
        return None;
    }
    // Bluetooth puts the VID source before the VID (`0002046D`), so the VID is the last 4 digits.
    Some(format!("{}:{}", &vid[vid.len() - 4..], &pid[..4]))
}

/// The hex digits right after the first marker found.
fn hex_after<'a>(id: &'a str, markers: [&str; 2]) -> Option<&'a str> {
    let start = markers
        .iter()
        .find_map(|marker| id.find(marker).map(|index| index + marker.len()))?;
    let rest = &id[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Windows menus read `&` as a mnemonic marker.
pub fn menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn hardware_ids_split_the_multi_string_and_stop_at_the_reported_length() {
        let mut buffer = utf16("ACPI\\VEN_PNP&DEV_0303\0*PNP0303\0\0");
        let bytes = (buffer.len() * 2) as u32;
        buffer.extend(utf16("stale\0"));
        assert_eq!(
            hardware_ids(&buffer, bytes),
            ids(&["ACPI\\VEN_PNP&DEV_0303", "*PNP0303"])
        );
    }

    #[test]
    fn hardware_ids_ignore_a_length_past_the_buffer() {
        let buffer = utf16("ACPI\\PNP0303");
        assert_eq!(hardware_ids(&buffer, 4096), ids(&["ACPI\\PNP0303"]));
    }

    #[test]
    fn ps2_and_i2c_keyboards_are_built_in_and_usb_is_not() {
        assert!(is_builtin("ACPI\\VEN_PNP&DEV_0303"));
        assert!(is_builtin("HID\\VEN_ELAN&DEV_0732&Col01"));
        assert!(!is_builtin("HID\\VID_048D&PID_C101&Col02"));
        assert!(!is_builtin("HID\\VID_046D&PID_C52B&MI_00"));
    }

    #[test]
    fn find_builtin_needs_exactly_one_match() {
        let usb = "HID\\VID_046D&PID_C52B&MI_00";
        let ps2 = "ACPI\\VEN_PNP&DEV_0303";
        assert_eq!(find_builtin(&ids(&[usb, ps2])), Some(1));
        assert_eq!(find_builtin(&ids(&[usb])), None);
        assert_eq!(find_builtin(&ids(&[ps2, usb, ps2])), None);
    }

    #[test]
    fn vid_pid_reads_usb_and_bluetooth_ids() {
        assert_eq!(
            vid_pid(r"HID\VID_3434&PID_0351&REV_0101&MI_00").as_deref(),
            Some("3434:0351")
        );
        assert_eq!(
            vid_pid(r"HID\{00001124-0000-1000-8000-00805f9b34fb}_VID&0002046d_PID&b342&Col01")
                .as_deref(),
            Some("046D:B342")
        );
        assert_eq!(
            vid_pid(r"HID\{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&02046d_PID&b023_REV&0001")
                .as_deref(),
            Some("046D:B023")
        );
    }

    #[test]
    fn vid_pid_rejects_ids_without_both_parts() {
        assert_eq!(vid_pid(r"ACPI\VEN_IDEA&DEV_0100"), None);
        assert_eq!(vid_pid(r"HID\VID_3434"), None);
        assert_eq!(vid_pid(r"HID\VID_34&PID_0351"), None);
    }

    #[test]
    fn menu_text_doubles_ampersands() {
        assert_eq!(menu_text("VEN_PNP&DEV_0303"), "VEN_PNP&&DEV_0303");
    }
}
