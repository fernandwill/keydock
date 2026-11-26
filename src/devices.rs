use std::ffi::c_void;
use windows::Win32::{
    Devices::{
        DeviceAndDriverInstallation::{
            CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_EVENT_DATA,
            CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_0, CM_NOTIFY_FILTER_0_0,
            CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification,
            CM_Unregister_Notification, CR_SUCCESS, DIGCF_PRESENT, GUID_DEVCLASS_KEYBOARD,
            HCMNOTIFICATION, HDEVINFO, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
            SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW,
        },
        HumanInterfaceDevice::GUID_DEVINTERFACE_KEYBOARD,
        Properties::{
            DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InLocalMachineContainer, DEVPROPTYPE,
        },
    },
    Foundation::{DEVPROPKEY, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
};
use windows::core::PCWSTR;

/// A keyboard interface that Windows reported arriving or leaving.
pub struct Change {
    pub arrived: bool,
    /// The interface path, which holds the hardware ID: `\\?\HID#VID_3434&PID_0351&MI_00#...`.
    pub link: String,
}

type Changed = Box<dyn Fn(Change) + Send + Sync>;

/// Calls `changed` on a system thread whenever a keyboard connects or disconnects, until dropped.
pub struct KeyboardWatch {
    handle: HCMNOTIFICATION,
    // Boxed twice so the callback gets a thin pointer; freed only after unregistering.
    _changed: Box<Changed>,
}

impl KeyboardWatch {
    pub fn start(changed: impl Fn(Change) + Send + Sync + 'static) -> Result<Self, String> {
        let changed: Box<Changed> = Box::new(Box::new(changed));
        let filter = CM_NOTIFY_FILTER {
            cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            u: CM_NOTIFY_FILTER_0 {
                DeviceInterface: CM_NOTIFY_FILTER_0_0 {
                    ClassGuid: GUID_DEVINTERFACE_KEYBOARD,
                },
            },
            ..Default::default()
        };
        let mut handle = HCMNOTIFICATION::default();
        let context: *const Changed = &*changed;
        let result = unsafe {
            CM_Register_Notification(
                &filter,
                Some(context.cast()),
                Some(on_keyboard_change),
                &mut handle,
            )
        };
        if result != CR_SUCCESS {
            return Err(format!(
                "CM_Register_Notification failed with code {}",
                result.0
            ));
        }
        Ok(Self {
            handle,
            _changed: changed,
        })
    }
}

impl Drop for KeyboardWatch {
    fn drop(&mut self) {
        // Waits for running callbacks, so `changed` outlives them. Nothing to do if it fails.
        let _ = unsafe { CM_Unregister_Notification(self.handle) };
    }
}

unsafe extern "system" fn on_keyboard_change(
    _handle: HCMNOTIFICATION,
    context: *const c_void,
    action: CM_NOTIFY_ACTION,
    data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    // Safe: `context` is the `Changed` that KeyboardWatch keeps alive until it unregisters, and
    // for interface events `data` holds a NUL-terminated SymbolicLink.
    let (changed, link) = unsafe {
        let link = &raw const (*data).u.DeviceInterface.SymbolicLink;
        (
            &*context.cast::<Changed>(),
            PCWSTR(link.cast()).to_string().unwrap_or_default(),
        )
    };
    changed(Change {
        arrived: action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL,
        link,
    });
    ERROR_SUCCESS.0
}

// Same size as the Interception buffer, in bytes.
const HARDWARE_ID_BYTES: usize = 1024;

struct DeviceList(HDEVINFO);

impl Drop for DeviceList {
    fn drop(&mut self) {
        // Nothing useful can happen if freeing the list fails.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// A keyboard that Windows lists as present.
#[derive(Clone, PartialEq)]
pub struct Present {
    /// The first hardware ID.
    pub id: String,
    /// Windows counts it as part of the PC.
    pub internal: bool,
}

/// Every keyboard that Windows lists as present. Unlike the Interception driver, Windows has no
/// limit on how often keyboards come and go.
pub fn present_keyboards() -> windows::core::Result<Vec<Present>> {
    let list = DeviceList(unsafe {
        SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_KEYBOARD), None, None, DIGCF_PRESENT)
    }?);
    let mut present = Vec::new();
    let mut device = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    for index in 0.. {
        match unsafe { SetupDiEnumDeviceInfo(list.0, index, &mut device) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
            Err(error) => return Err(error),
        }
        let mut local = [0u8; 1];
        // A device without the property is not counted as internal.
        let internal = property(
            &list,
            &device,
            &DEVPKEY_Device_InLocalMachineContainer,
            &mut local,
        )
        .is_some()
            && local[0] != 0;
        let mut ids = [0u8; HARDWARE_ID_BYTES];
        if let Some(bytes) = property(&list, &device, &DEVPKEY_Device_HardwareIds, &mut ids) {
            let units: Vec<u16> = ids[..bytes]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            if let Some(id) = crate::interception::hardware_ids(&units, bytes as u32).first() {
                present.push(Present {
                    id: id.clone(),
                    internal,
                });
            }
        }
    }
    Ok(present)
}

/// Reads a device property into `buffer`; returns the bytes written.
fn property(
    list: &DeviceList,
    device: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
    buffer: &mut [u8],
) -> Option<usize> {
    let mut kind = DEVPROPTYPE::default();
    let mut size = 0u32;
    unsafe {
        SetupDiGetDevicePropertyW(
            list.0,
            device,
            key,
            &mut kind,
            Some(buffer),
            Some(&mut size),
            0,
        )
    }
    .ok()?;
    Some(size as usize)
}
