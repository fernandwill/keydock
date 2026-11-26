use std::{
    ffi::c_void,
    fmt,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
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
const FILTER_NONE: u16 = 0x0000;
const FILTER_KEY_ALL: u16 = 0xFFFF;
const KEY_UP: u16 = 0x01;
// Ample for a scan, which takes a few milliseconds plus at most one wait of POLL_MS.
const SCAN_TIMEOUT: Duration = Duration::from_secs(2);
// While blocking, how long the input thread waits for a key before it checks for a new device set.
const POLL_MS: u32 = 50;

type Proc = unsafe extern "system" fn() -> isize;
type CreateContext = unsafe extern "C" fn() -> *mut c_void;
type DestroyContext = unsafe extern "C" fn(*mut c_void);
type GetHardwareId = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, u32) -> u32;
type Predicate = extern "C" fn(i32) -> i32;
type SetFilter = unsafe extern "C" fn(*mut c_void, Predicate, u16);
type Wait = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type Receive = unsafe extern "C" fn(*mut c_void, i32, *mut KeyStroke, u32) -> i32;
type Send = unsafe extern "C" fn(*mut c_void, i32, *const KeyStroke, u32) -> i32;

/// A set of Interception devices: bit n stands for device n.
pub type Devices = u32;

pub fn device_bit(device: i32) -> Devices {
    1 << device
}

// interception_set_filter's predicate takes no user data, so the device set travels in a static.
// Only the input thread sets filters, so nothing races on it.
static FILTER_DEVICES: AtomicU32 = AtomicU32::new(0);

extern "C" fn in_filter_devices(device: i32) -> i32 {
    KEYBOARD_DEVICES.contains(&device) as i32
        & (FILTER_DEVICES.load(Ordering::Relaxed) >> device) as i32
        & 1
}

/// Interception's InterceptionKeyStroke.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KeyStroke {
    code: u16,
    state: u16,
    information: u32,
}

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

impl Library {
    /// `name` must end with a NUL.
    fn function(&self, name: &'static str) -> Result<Proc, ScanError> {
        unsafe { GetProcAddress(self.0, PCSTR(name.as_ptr())) }
            .ok_or(ScanError::MissingFunction(name.trim_end_matches('\0')))
    }
}

/// Closed when dropped, also on a panic. Closing it also clears every filter it set.
struct Context {
    raw: *mut c_void,
    destroy: DestroyContext,
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.raw) };
    }
}

#[derive(Clone, PartialEq)]
pub struct Keyboard {
    pub device: i32,
    /// The first hardware ID.
    pub id: String,
}

/// keydock's only connection to the driver. Each connection registers its own wake-up events,
/// so a second one could take keys meant for the first.
pub struct Driver {
    // Fields drop in order: the context closes before the DLL unloads.
    context: Context,
    _library: Library,
    get_hardware_id: GetHardwareId,
    set_filter: SetFilter,
    wait: Wait,
    receive: Receive,
    send: Send,
}

// Safe: the context is a heap block of handles that any thread may use, and keydock uses a Driver
// from one thread at a time (the main thread for the scan, then only the input thread).
unsafe impl std::marker::Send for Driver {}

impl Driver {
    pub fn open() -> Result<Self, ScanError> {
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
        unsafe {
            let create = std::mem::transmute::<Proc, CreateContext>(
                library.function("interception_create_context\0")?,
            );
            let destroy = std::mem::transmute::<Proc, DestroyContext>(
                library.function("interception_destroy_context\0")?,
            );
            let get_hardware_id = std::mem::transmute::<Proc, GetHardwareId>(
                library.function("interception_get_hardware_id\0")?,
            );
            let set_filter = std::mem::transmute::<Proc, SetFilter>(
                library.function("interception_set_filter\0")?,
            );
            let wait = std::mem::transmute::<Proc, Wait>(
                library.function("interception_wait_with_timeout\0")?,
            );
            let receive =
                std::mem::transmute::<Proc, Receive>(library.function("interception_receive\0")?);
            let send = std::mem::transmute::<Proc, Send>(library.function("interception_send\0")?);
            let raw = create();
            if raw.is_null() {
                return Err(ScanError::DriverUnavailable);
            }
            Ok(Self {
                context: Context { raw, destroy },
                _library: library,
                get_hardware_id,
                set_filter,
                wait,
                receive,
                send,
            })
        }
    }

    /// Every connected keyboard. Changes no filter, so input is unchanged.
    pub fn keyboards(&self) -> Vec<Keyboard> {
        let mut keyboards = Vec::new();
        let mut buffer = [0u16; HARDWARE_ID_UNITS];
        for device in KEYBOARD_DEVICES {
            let bytes = unsafe {
                (self.get_hardware_id)(
                    self.context.raw,
                    device,
                    buffer.as_mut_ptr().cast(),
                    size_of_val(&buffer) as u32,
                )
            };
            // An empty slot or an ID longer than the buffer returns 0 bytes.
            if let Some(id) = hardware_ids(&buffer, bytes).into_iter().next() {
                keyboards.push(Keyboard { device, id });
            }
        }
        keyboards
    }

    fn set_filter(&self, devices: Devices, filter: u16) {
        if devices == 0 {
            return;
        }
        FILTER_DEVICES.store(devices, Ordering::Relaxed);
        unsafe { (self.set_filter)(self.context.raw, in_filter_devices, filter) };
    }

    /// The device with a key waiting, or `None` after `POLL_MS`.
    fn wait(&self) -> Option<i32> {
        let device = unsafe { (self.wait)(self.context.raw, POLL_MS) };
        (device > 0).then_some(device)
    }

    fn receive(&self, device: i32) -> Option<KeyStroke> {
        let mut stroke = KeyStroke::default();
        let count = unsafe { (self.receive)(self.context.raw, device, &mut stroke, 1) };
        (count > 0).then_some(stroke)
    }

    fn send(&self, device: i32, stroke: &KeyStroke) {
        // A lost key-up only leaves a key down until it is pressed again; nothing to recover here.
        unsafe { (self.send)(self.context.raw, device, stroke, 1) };
    }
}

enum Request {
    Block(Devices),
    Scan(Sender<Vec<Keyboard>>),
}

/// The input thread. It owns the driver, blocks the devices in the last set it was sent, and
/// scans for keyboards on request.
pub struct Blocker {
    requests: Option<Sender<Request>>,
    thread: Option<JoinHandle<()>>,
}

impl Blocker {
    /// First clears any filter left on `built_in`, so keydock always starts with it working.
    /// `stopped` runs when the thread ends, also after a panic.
    pub fn spawn(
        driver: Driver,
        built_in: Devices,
        stopped: impl Fn() + std::marker::Send + 'static,
    ) -> Self {
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _stopped = OnDrop(stopped);
            block_loop(&driver, built_in, &requests);
        });
        Self {
            requests: Some(sender),
            thread: Some(thread),
        }
    }

    pub fn block(&self, devices: Devices) {
        if let Some(requests) = &self.requests {
            // Fails only if the thread panicked; its driver closed then, which cleared the filters.
            let _ = requests.send(Request::Block(devices));
        }
    }

    /// Every connected keyboard, or `None` if the input thread has stopped.
    pub fn keyboards(&self) -> Option<Vec<Keyboard>> {
        let (reply, keyboards) = mpsc::channel();
        self.requests.as_ref()?.send(Request::Scan(reply)).ok()?;
        keyboards.recv_timeout(SCAN_TIMEOUT).ok()
    }

    pub fn stopped(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

struct OnDrop<F: Fn()>(F);

impl<F: Fn()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        // Closing the channel tells the thread to clear its filters and stop.
        self.requests = None;
        if let Some(thread) = self.thread.take() {
            // A panic in the thread already closed its driver; there is nothing left to undo.
            let _ = thread.join();
        }
    }
}

fn block_loop(driver: &Driver, built_in: Devices, requests: &Receiver<Request>) {
    driver.set_filter(built_in, FILTER_NONE);
    let mut blocked: Devices = 0;
    loop {
        // With nothing filtered no key can arrive, so the thread sleeps until the next request.
        // While blocking it must poll: the driver's wait cannot be woken by a request.
        let request = if blocked == 0 {
            requests.recv().map_err(|_| TryRecvError::Disconnected)
        } else {
            requests.try_recv()
        };
        match request {
            Ok(Request::Scan(reply)) => {
                // The caller gave up after SCAN_TIMEOUT if this fails; nothing to do.
                let _ = reply.send(driver.keyboards());
                continue;
            }
            Ok(Request::Block(devices)) => {
                driver.set_filter(blocked & !devices, FILTER_NONE);
                driver.set_filter(devices, FILTER_KEY_ALL);
                blocked = devices;
                continue;
            }
            Err(TryRecvError::Disconnected) => {
                driver.set_filter(blocked, FILTER_NONE);
                return;
            }
            Err(TryRecvError::Empty) => {}
        }
        let Some(device) = driver.wait() else {
            continue;
        };
        let Some(stroke) = driver.receive(device) else {
            continue;
        };
        // Key-ups pass, so a key held down when blocking starts is not left down in Windows.
        // Keys queued before an unblock also pass.
        if blocked & device_bit(device) == 0 || stroke.state & KEY_UP != 0 {
            driver.send(device, &stroke);
        }
    }
}

/// Turns a REG_MULTI_SZ buffer of `bytes` length into its strings.
pub fn hardware_ids(buffer: &[u16], bytes: u32) -> Vec<String> {
    let units = (bytes as usize / 2).min(buffer.len());
    buffer[..units]
        .split(|&unit| unit == 0)
        .filter(|id| !id.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
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
    fn only_listed_devices_pass_the_filter_predicate() {
        FILTER_DEVICES.store(device_bit(2) | device_bit(5), Ordering::Relaxed);
        let passed: Vec<i32> = (0..=20).filter(|&d| in_filter_devices(d) == 1).collect();
        assert_eq!(passed, [2, 5]);
        FILTER_DEVICES.store(0, Ordering::Relaxed);
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
