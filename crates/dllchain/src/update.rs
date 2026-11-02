//! Asking GitHub for dllchain's newest release, only when the player asks. One request for a version number: nothing
//! about the PC or the game is sent, and nothing is downloaded or run.

use std::ffi::c_void;
use std::ptr::null;
use windows_sys::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};

pub const THIS: &str = env!("CARGO_PKG_VERSION");
const API_HOST: &str = "api.github.com";
const API_PATH: &str = "/repos/AKJama/dllchain/releases/latest";
const RELEASES: &str = "https://github.com/AKJama/dllchain/releases";

/// The check's answer, or why there is none.
pub type Answer = Result<Found, String>;

/// What the check found.
#[derive(Debug, PartialEq)]
pub enum Found {
    UpToDate,
    /// A newer release: its version and its page on GitHub.
    Newer(String, String),
}

/// Asks GitHub for the newest published release. Takes up to a few seconds; run it off the window's thread.
pub fn check() -> Answer {
    let body = get(API_HOST, API_PATH)?;
    let tag = tag_in(&String::from_utf8_lossy(&body)).ok_or("GitHub's answer had no version in it.")?;
    compare(THIS, &tag)
}

/// The words for what the check found, for the window and the command line (which adds the page link).
pub fn describe(r: &Answer) -> String {
    match r {
        Ok(Found::UpToDate) => format!("dllchain {THIS} is the newest version."),
        Ok(Found::Newer(v, _)) => format!(
            "dllchain {v} is out (this is {THIS}).\n\nDownload its zip, unpack it, and use it instead of this one. \
             Your mods stay as they are."
        ),
        Err(e) => format!("Could not check for a newer dllchain: {e}\n\nThe releases are at {RELEASES}"),
    }
}

/// The newer release, if `tag` is one. The page link is built here, never taken from the answer.
fn compare(this: &str, tag: &str) -> Result<Found, String> {
    let theirs = version(tag).ok_or_else(|| format!("GitHub named an odd version, {tag:?}."))?;
    let ours = version(this).expect("our own version is x.y.z");
    Ok(if theirs > ours {
        let v = format!("{}.{}.{}", theirs.0, theirs.1, theirs.2);
        Found::Newer(v.clone(), format!("{RELEASES}/tag/v{v}"))
    } else {
        Found::UpToDate
    })
}

/// `v1.2.3` or `1.2.3`, nothing else.
fn version(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.strip_prefix('v').unwrap_or(s).split('.');
    let mut next = || parts.next().filter(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()));
    let v = (next()?.parse().ok()?, next()?.parse().ok()?, next()?.parse().ok()?);
    parts.next().is_none().then_some(v)
}

/// The `tag_name` of GitHub's release answer, without a JSON parser: the first `"tag_name": "..."`.
fn tag_in(json: &str) -> Option<String> {
    let rest = &json[json.find("\"tag_name\"")? + "\"tag_name\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let tag = &rest[..rest.find('"')?];
    (tag.len() <= 32).then(|| tag.to_string())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Closes a WinHTTP handle when it goes out of scope.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle WinHTTP gave us, closed once.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn open(h: *mut c_void, what: &str) -> Result<Handle, String> {
    if h.is_null() {
        Err(format!("{what} failed (Windows error {}).", std::io::Error::last_os_error().raw_os_error().unwrap_or(0)))
    } else {
        Ok(Handle(h))
    }
}

/// An HTTPS GET with Windows' own HTTP stack and proxy settings; at most 1 MB of answer.
fn get(host: &str, path: &str) -> Result<Vec<u8>, String> {
    let agent = wide(&format!("dllchain/{THIS}"));
    let (host, path, verb) = (wide(host), wide(path), wide("GET"));
    let headers = wide("Accept: application/vnd.github+json\r\n");
    // SAFETY: NUL-terminated strings that outlive the calls, and handles closed by `Handle`.
    unsafe {
        let session = open(
            WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0),
            "Starting Windows' HTTP",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 10_000, 10_000);
        let conn = open(WinHttpConnect(session.0, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0), "Reaching GitHub")?;
        let req = open(
            WinHttpOpenRequest(conn.0, verb.as_ptr(), path.as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE),
            "Asking GitHub",
        )?;
        if WinHttpSendRequest(req.0, headers.as_ptr(), u32::MAX, null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0
        {
            return Err("GitHub could not be reached. Is this PC online?".into());
        }
        let (mut code, mut len) = (0u32, 4u32);
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&raw mut code).cast(),
            &mut len,
            std::ptr::null_mut(),
        );
        if code != 200 {
            return Err(format!("GitHub answered with HTTP {code}."));
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let mut n = 0u32;
            if WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n) == 0 {
                return Err("GitHub's answer was cut off.".into());
            }
            if n == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > 1 << 20 {
                return Err("GitHub's answer was far too long.".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_as_numbers() {
        assert_eq!(compare("0.1.1", "v0.1.1"), Ok(Found::UpToDate));
        assert_eq!(compare("0.1.10", "v0.1.9"), Ok(Found::UpToDate));
        assert_eq!(
            compare("0.1.9", "v0.1.10"),
            Ok(Found::Newer("0.1.10".into(), "https://github.com/AKJama/dllchain/releases/tag/v0.1.10".into()))
        );
        assert_eq!(compare("0.1.1", "1.0.0").map(|f| matches!(f, Found::Newer(..))), Ok(true));
        // the page is built from the numbers, so an odd tag cannot point the player elsewhere
        for odd in ["v0.2.0/../../evil", "v0.2", "v0.2.0.1", "v0.2.0-rc1", "", "v+1.0.0", "v0.2.0 https://x"] {
            assert!(compare("0.1.1", odd).is_err(), "{odd}");
        }
    }

    #[test]
    fn the_tag_is_read_from_githubs_answer() {
        let json = r#"{"url":"https://api.github.com/x","tag_name" : "v0.1.2","name":"dllchain v0.1.2"}"#;
        assert_eq!(tag_in(json).as_deref(), Some("v0.1.2"));
        assert_eq!(tag_in(r#"{"message":"Not Found"}"#), None);
        assert_eq!(tag_in(r#"{"tag_name":"v0.1.2"#), None);
    }
}
