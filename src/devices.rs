use std::collections::HashSet;
use windows::Win32::{
    Devices::{
        DeviceAndDriverInstallation::{
            DIGCF_PRESENT, GUID_DEVCLASS_KEYBOARD, HDEVINFO, SP_DEVINFO_DATA,
            SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
            SetupDiGetDevicePropertyW,
        },
        Properties::{
            DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InLocalMachineContainer, DEVPROPTYPE,
        },
    },
    Foundation::{DEVPROPKEY, ERROR_NO_MORE_ITEMS},
};

// Same size as the Interception buffer, in bytes.
const HARDWARE_ID_BYTES: usize = 1024;

struct DeviceList(HDEVINFO);

impl Drop for DeviceList {
    fn drop(&mut self) {
        // Nothing useful can happen if freeing the list fails.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// First hardware IDs, in upper case, of present keyboards that Windows counts as part of the PC.
pub fn internal_keyboards() -> windows::core::Result<HashSet<String>> {
    let list = DeviceList(unsafe {
        SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_KEYBOARD), None, None, DIGCF_PRESENT)
    }?);
    let mut internal = HashSet::new();
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
        if property(
            &list,
            &device,
            &DEVPKEY_Device_InLocalMachineContainer,
            &mut local,
        )
        .is_none()
            || local[0] == 0
        {
            continue;
        }
        let mut ids = [0u8; HARDWARE_ID_BYTES];
        if let Some(bytes) = property(&list, &device, &DEVPKEY_Device_HardwareIds, &mut ids) {
            let units: Vec<u16> = ids[..bytes]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            if let Some(id) = crate::interception::hardware_ids(&units, bytes as u32).first() {
                internal.insert(id.to_ascii_uppercase());
            }
        }
    }
    Ok(internal)
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
