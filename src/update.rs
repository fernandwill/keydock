use std::{ffi::c_void, ptr, sync::mpsc::Sender, thread, time::Duration};
use windows::{
    Win32::Networking::WinHttp::{
        INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
        WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
        WinHttpReceiveResponse, WinHttpSendRequest,
    },
    core::{Error, HSTRING, PCWSTR, w},
};

pub const RELEASES_PAGE: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/latest");
const USER_AGENT: &str = concat!("keydock/", env!("CARGO_PKG_VERSION"));
// Delayed so that startup, and the short-lived keydocks in tests/tray_quit.py, make no request.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
// tag_name sits near the start of the release JSON, so a long asset list can be cut off.
const MAX_BODY_BYTES: usize = 1 << 20;
const READ_CHUNK_BYTES: usize = 8192;

/// Checks for a newer release after FIRST_CHECK_DELAY and then daily. `wake` runs after each send.
pub fn spawn_checker(results: Sender<Outcome>, wake: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK_DELAY);
        loop {
            let result = match api_path() {
                None => Outcome::Failed("the repository is not on GitHub".into()),
                Some(path) => match fetch(&path) {
                    Ok((status, body)) => outcome(status, &body, env!("CARGO_PKG_VERSION")),
                    Err(error) => Outcome::Failed(describe(&error)),
                },
            };
            if results.send(result).is_err() {
                return;
            }
            wake();
            thread::sleep(CHECK_INTERVAL);
        }
    });
}

fn describe(error: &Error) -> String {
    let message = error.message();
    match message.trim() {
        "" => format!("error {}", error.code()),
        text => text.to_string(),
    }
}

fn api_path() -> Option<String> {
    let repository = env!("CARGO_PKG_REPOSITORY").strip_prefix("https://github.com/")?;
    Some(format!(
        "/repos/{}/releases/latest",
        repository.trim_end_matches('/')
    ))
}

struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> windows::core::Result<Self> {
        if raw.is_null() {
            Err(Error::from_thread())
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Nothing useful can happen if closing fails; the handle is gone either way.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn fetch(path: &str) -> windows::core::Result<(u32, String)> {
    unsafe {
        let session = Handle::new(WinHttpOpen(
            &HSTRING::from(USER_AGENT),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        let connection = Handle::new(WinHttpConnect(
            session.0,
            w!("api.github.com"),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let headers: Vec<u16> = "Accept: application/vnd.github+json"
            .encode_utf16()
            .collect();
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, ptr::null_mut())?;
        let mut status = 0u32;
        let mut status_size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut status_size,
            ptr::null_mut(),
        )?;
        let mut body = Vec::new();
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        while body.len() < MAX_BODY_BYTES {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    UpToDate,
    Available(String),
    Failed(String),
}

const HTTP_OK: u32 = 200;
// GitHub answers 404 while the repository has no release yet.
const HTTP_NOT_FOUND: u32 = 404;

/// Judges a response from GitHub's "latest release" endpoint against the running version.
pub fn outcome(status: u32, body: &str, current: &str) -> Outcome {
    match status {
        HTTP_OK => {}
        HTTP_NOT_FOUND => return Outcome::UpToDate,
        _ => return Outcome::Failed(format!("GitHub answered HTTP {status}")),
    }
    let Some(tag) = tag_name(body) else {
        return Outcome::Failed("the release has no tag".into());
    };
    match (version(tag), version(current)) {
        (Some(latest), Some(running)) if latest > running => Outcome::Available(tag.into()),
        (Some(_), Some(_)) => Outcome::UpToDate,
        _ => Outcome::Failed(format!("unrecognized version {tag:?}")),
    }
}

fn tag_name(body: &str) -> Option<&str> {
    let (_, rest) = body.split_once(r#""tag_name""#)?;
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    rest.split_once('"').map(|(tag, _)| tag)
}

fn version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .split('.')
        .map(|part| part.parse().ok());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => {
            Some((major, minor, patch))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> String {
        format!(r#"{{"url":"https://api.github.com/x","tag_name":"{tag}","name":"keydock {tag}"}}"#)
    }

    #[test]
    fn winhttp_errors_without_a_system_message_show_their_code() {
        // ERROR_WINHTTP_NAME_NOT_RESOLVED; Windows' message table does not hold WinHTTP texts.
        let error = Error::from_hresult(windows::core::HRESULT(0x8007_2EE7_u32 as i32));
        assert!(
            describe(&error).contains("0x80072EE7"),
            "{:?}",
            describe(&error)
        );
    }

    #[test]
    fn repository_points_at_the_github_api() {
        assert_eq!(
            api_path().as_deref(),
            Some("/repos/fernandwill/keydock/releases/latest")
        );
    }

    #[test]
    fn newer_release_is_available() {
        assert_eq!(
            outcome(200, &release("v0.2.0"), "0.1.0"),
            Outcome::Available("v0.2.0".into())
        );
    }

    #[test]
    fn versions_compare_as_numbers_not_text() {
        let spaced = r#"{ "tag_name": "v0.10.0" }"#;
        assert_eq!(
            outcome(200, spaced, "0.9.0"),
            Outcome::Available("v0.10.0".into())
        );
    }

    #[test]
    fn same_or_older_release_is_up_to_date() {
        assert_eq!(outcome(200, &release("v0.1.0"), "0.1.0"), Outcome::UpToDate);
        assert_eq!(outcome(200, &release("v0.0.9"), "0.1.0"), Outcome::UpToDate);
    }

    #[test]
    fn no_release_yet_is_up_to_date() {
        assert_eq!(
            outcome(404, r#"{"message":"Not Found"}"#, "0.1.0"),
            Outcome::UpToDate
        );
    }

    #[test]
    fn other_statuses_fail_with_the_status() {
        let Outcome::Failed(reason) =
            outcome(403, r#"{"message":"API rate limit exceeded"}"#, "0.1.0")
        else {
            panic!("403 must fail");
        };
        assert!(reason.contains("403"), "{reason}");
    }

    #[test]
    fn unreadable_tags_fail() {
        assert!(matches!(
            outcome(200, r#"{"name":"x"}"#, "0.1.0"),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            outcome(200, &release("v1.2"), "0.1.0"),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            outcome(200, &release("v1.2.3-rc1"), "0.1.0"),
            Outcome::Failed(_)
        ));
    }
}
