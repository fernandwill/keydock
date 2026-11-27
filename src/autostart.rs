use std::{io, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
            RegSetKeyValueW,
        },
    },
    core::{PCWSTR, w},
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: PCWSTR = w!("keydock");

/// Whether Windows starts this keydock.exe at login. A value for another path, left by a moved
/// folder, counts as off.
pub fn enabled() -> io::Result<bool> {
    Ok(read(RUN_KEY)? == Some(command()?))
}

pub fn set(on: bool) -> io::Result<()> {
    match on {
        true => write(RUN_KEY, Some(&command()?)),
        false => write(RUN_KEY, None),
    }
}

/// The running exe, quoted because the path may hold spaces. No trailing NUL.
fn command() -> io::Result<Vec<u16>> {
    let exe = std::env::current_exe()?;
    Ok("\""
        .encode_utf16()
        .chain(exe.as_os_str().encode_wide())
        .chain("\"".encode_utf16())
        .collect())
}

/// The keydock value under `key` in HKCU without its trailing NUL; `None` if key or value is missing.
fn read(key: PCWSTR) -> io::Result<Option<Vec<u16>>> {
    let mut bytes = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok()?;
    let mut buffer = vec![0u16; bytes as usize / 2];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    }
    .ok()?;
    buffer.truncate(bytes as usize / 2);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    Ok(Some(buffer))
}

/// Writes `command` as the keydock value under `key` in HKCU, or deletes the value for `None`.
fn write(key: PCWSTR, command: Option<&[u16]>) -> io::Result<()> {
    let Some(command) = command else {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, VALUE) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        return Ok(status.ok()?);
    };
    let data: Vec<u16> = command.iter().copied().chain([0]).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key,
            VALUE,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            size_of_val(data.as_slice()) as u32,
        )
    }
    .ok()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{Win32::System::Registry::RegDeleteKeyW, core::HSTRING};

    #[test]
    fn a_written_command_reads_back_whole_and_deleting_twice_is_fine() {
        let name = HSTRING::from(format!(r"Software\keydock-test-{}", std::process::id()));
        let key = PCWSTR(name.as_ptr());
        let command: Vec<u16> =
            r#""C:\Program Files\keydock\keydock.exe""#.encode_utf16().collect();
        assert_eq!(read(key).unwrap(), None, "missing key");
        write(key, Some(&command)).unwrap();
        let back = read(key).unwrap();
        write(key, None).unwrap();
        write(key, None).unwrap();
        let gone = read(key).unwrap();
        let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key) };
        assert_eq!(
            back.map(|units| String::from_utf16_lossy(&units)),
            Some(String::from_utf16_lossy(&command))
        );
        assert_eq!(gone, None);
    }
}
