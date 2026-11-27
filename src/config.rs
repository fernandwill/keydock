use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::{OsString, c_void},
    fs,
    io::{self, Write},
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows::Win32::{
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
};

/// `%APPDATA%\keydock\config.toml`. Unknown keys are ignored, so older keydocks read newer files.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// VID:PID of the keyboards that trigger the block, for example `3434:0351`.
    #[serde(default)]
    pub allowlist: BTreeSet<String>,
    /// "Pause keydock": the built-in keyboard stays on until the user resumes.
    #[serde(default)]
    pub paused: bool,
}

pub fn path() -> windows::core::Result<PathBuf> {
    let folder = unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None)?;
        let folder = OsString::from_wide(raw.as_wide());
        CoTaskMemFree(Some(raw.0 as *const c_void));
        folder
    };
    Ok(PathBuf::from(folder).join("keydock").join("config.toml"))
}

/// A missing file is an empty config; an unreadable one is an error.
pub fn load(path: &Path) -> Result<Config, String> {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(format!("config.toml could not be read: {error}")),
        Ok(text) => toml::from_str(&text)
            .map_err(|error| format!("config.toml is not valid: {}", error.message())),
    }
}

pub fn save(path: &Path, config: &Config) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let text = toml::to_string(config).map_err(io::Error::other)?;
    // Write, flush to disk, then rename, so a crash or power cut never leaves a half-written config.
    let temp = path.with_extension("toml.tmp");
    let mut file = fs::File::create(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keydock-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saved_config_loads_back_and_creates_the_folder() {
        let path = scratch("roundtrip").join("config.toml");
        let config = Config {
            allowlist: BTreeSet::from(["3434:0351".to_string()]),
            paused: true,
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
    }

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let path = scratch("missing").join("config.toml");
        assert_eq!(load(&path).unwrap(), Config::default());
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let dir = scratch("broken");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "allowlist = [").unwrap();
        assert!(load(&path).is_err());
    }
}
