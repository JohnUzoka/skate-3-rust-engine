//! [`Transport`] over libcurl's easy interface: devkitPro's switch-curl on the Switch, built with
//! libnx's TLS backend, so certificates are checked by the console's SSL service against its
//! trusted CAs and clock; the system's libcurl on a PC (`system-curl`).
//!
//! Only what the updater calls is declared, with the numbers from libcurl 7.69.1's `curl.h`, the
//! version devkitPro packages. They are libcurl's stable ABI. `curl_global_init` opens the SSL
//! service on the Switch and `curl_global_cleanup` closes it, so each request does both: nothing
//! stays open between requests, or at exit.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_long, c_void},
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{Error, Transport};

type Code = c_int;

const CURL_GLOBAL_DEFAULT: c_long = 3;
const CURLE_OK: Code = 0;
const CURLE_HTTP_RETURNED_ERROR: Code = 22;
const CURLE_WRITE_ERROR: Code = 23;
const CURLE_ABORTED_BY_CALLBACK: Code = 42;
const CURLE_FILESIZE_EXCEEDED: Code = 63;
const CURL_ERROR_SIZE: usize = 256;
const CURLPROTO_HTTPS: c_long = 1 << 1;
const CURLINFO_RESPONSE_CODE: c_int = 0x200000 + 2;

const CURLOPT_WRITEDATA: c_int = 10001;
const CURLOPT_URL: c_int = 10002;
const CURLOPT_ERRORBUFFER: c_int = 10010;
const CURLOPT_WRITEFUNCTION: c_int = 20011;
const CURLOPT_USERAGENT: c_int = 10018;
const CURLOPT_LOW_SPEED_LIMIT: c_int = 19;
const CURLOPT_LOW_SPEED_TIME: c_int = 20;
const CURLOPT_HTTPHEADER: c_int = 10023;
const CURLOPT_NOPROGRESS: c_int = 43;
const CURLOPT_FAILONERROR: c_int = 45;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_XFERINFODATA: c_int = 10057;
const CURLOPT_SSL_VERIFYPEER: c_int = 64;
const CURLOPT_MAXREDIRS: c_int = 68;
const CURLOPT_CONNECTTIMEOUT: c_int = 78;
const CURLOPT_SSL_VERIFYHOST: c_int = 81;
const CURLOPT_NOSIGNAL: c_int = 99;
const CURLOPT_MAXFILESIZE_LARGE: c_int = 30117;
const CURLOPT_PROTOCOLS: c_int = 181;
const CURLOPT_REDIR_PROTOCOLS: c_int = 182;
const CURLOPT_XFERINFOFUNCTION: c_int = 20219;

/// Give up connecting after this many seconds.
const CONNECT_TIMEOUT: c_long = 15;
/// Give up on a transfer that stays below 1 KiB/s for this many seconds.
const STALL_TIME: c_long = 30;

unsafe extern "C" {
    fn curl_global_init(flags: c_long) -> Code;
    fn curl_global_cleanup();
    fn curl_easy_init() -> *mut c_void;
    fn curl_easy_setopt(handle: *mut c_void, option: c_int, ...) -> Code;
    fn curl_easy_perform(handle: *mut c_void) -> Code;
    fn curl_easy_getinfo(handle: *mut c_void, info: c_int, ...) -> Code;
    fn curl_easy_cleanup(handle: *mut c_void);
    fn curl_easy_strerror(code: Code) -> *const c_char;
    fn curl_slist_append(list: *mut c_void, header: *const c_char) -> *mut c_void;
    fn curl_slist_free_all(list: *mut c_void);
}

/// libcurl, identifying itself to GitHub as `user_agent` (its API requires one).
pub struct Curl {
    user_agent: CString,
}

impl Curl {
    pub fn new(user_agent: &str) -> Self {
        Self { user_agent: CString::new(user_agent.replace('\0', "")).unwrap_or_default() }
    }
}

/// What the callbacks share with `get`.
struct Transfer<'a> {
    sink: &'a mut dyn Write,
    limit: u64,
    received: u64,
    cancel: &'a AtomicBool,
    /// Why the write callback stopped the transfer.
    failure: Option<String>,
}

extern "C" fn write(data: *const c_char, size: usize, count: usize, transfer: *mut c_void) -> usize {
    // SAFETY: CURLOPT_WRITEDATA is the `Transfer` that `get` keeps alive through the transfer.
    let transfer = unsafe { &mut *transfer.cast::<Transfer>() };
    let Some(length) = size.checked_mul(count) else { return 0 };
    if transfer.cancel.load(Ordering::Relaxed) {
        return 0;
    }
    if transfer.received + length as u64 > transfer.limit {
        transfer.failure = Some(format!("it is longer than the {} bytes expected", transfer.limit));
        return 0;
    }
    // SAFETY: libcurl hands over `length` bytes at `data`.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
    match transfer.sink.write_all(bytes) {
        Ok(()) => {
            transfer.received += length as u64;
            length
        }
        Err(e) => {
            transfer.failure = Some(format!("couldn't save it: {e}"));
            0
        }
    }
}

extern "C" fn progress(transfer: *mut c_void, _: i64, _: i64, _: i64, _: i64) -> c_int {
    // SAFETY: CURLOPT_XFERINFODATA is the same live `Transfer`; this only reads its flag.
    let transfer = unsafe { &*transfer.cast::<Transfer>() };
    transfer.cancel.load(Ordering::Relaxed).into()
}

impl Transport for Curl {
    fn get(&self, url: &str, accept: &str, limit: u64, sink: &mut dyn Write, cancel: &AtomicBool) -> Result<(), Error> {
        let (Ok(url), Ok(accept)) = (CString::new(url), CString::new(format!("Accept: {accept}"))) else {
            return Err(Error::Failed("a URL with a NUL in it".into()));
        };
        let mut transfer = Transfer { sink, limit, received: 0, cancel, failure: None };
        // SAFETY: init and cleanup pair up around the request, and only one thread uses libcurl.
        unsafe {
            if curl_global_init(CURL_GLOBAL_DEFAULT) != CURLE_OK {
                return Err(Error::Failed("libcurl couldn't start".into()));
            }
            let result = perform(self, &url, &accept, &mut transfer);
            curl_global_cleanup();
            result
        }
    }
}

/// One GET with a fresh easy handle.
///
/// # Safety
/// Between `curl_global_init` and `curl_global_cleanup`.
unsafe fn perform(curl: &Curl, url: &CStr, accept: &CStr, transfer: &mut Transfer) -> Result<(), Error> {
    let handle = unsafe { curl_easy_init() };
    if handle.is_null() {
        return Err(Error::Failed("libcurl couldn't make a request".into()));
    }
    let mut headers = unsafe { curl_slist_append(std::ptr::null_mut(), accept.as_ptr()) };
    headers = unsafe { curl_slist_append(headers, c"X-GitHub-Api-Version: 2022-11-28".as_ptr()) };
    let mut message = [0 as c_char; CURL_ERROR_SIZE];
    let data = std::ptr::from_mut(transfer).cast::<c_void>();
    // SAFETY: each option gets the type curl.h gives it: `long` for numbers, `curl_off_t` (64
    // bits) for the size, and pointers that outlive the transfer.
    let options = unsafe {
        [
            curl_easy_setopt(handle, CURLOPT_URL, url.as_ptr()),
            curl_easy_setopt(handle, CURLOPT_HTTPHEADER, headers),
            curl_easy_setopt(handle, CURLOPT_USERAGENT, curl.user_agent.as_ptr()),
            curl_easy_setopt(handle, CURLOPT_PROTOCOLS, CURLPROTO_HTTPS),
            curl_easy_setopt(handle, CURLOPT_REDIR_PROTOCOLS, CURLPROTO_HTTPS),
            curl_easy_setopt(handle, CURLOPT_FOLLOWLOCATION, 1 as c_long),
            curl_easy_setopt(handle, CURLOPT_MAXREDIRS, 5 as c_long),
            curl_easy_setopt(handle, CURLOPT_SSL_VERIFYPEER, 1 as c_long),
            curl_easy_setopt(handle, CURLOPT_SSL_VERIFYHOST, 2 as c_long),
            curl_easy_setopt(handle, CURLOPT_FAILONERROR, 1 as c_long),
            curl_easy_setopt(handle, CURLOPT_NOSIGNAL, 1 as c_long),
            curl_easy_setopt(handle, CURLOPT_CONNECTTIMEOUT, CONNECT_TIMEOUT),
            curl_easy_setopt(handle, CURLOPT_LOW_SPEED_LIMIT, 1024 as c_long),
            curl_easy_setopt(handle, CURLOPT_LOW_SPEED_TIME, STALL_TIME),
            curl_easy_setopt(handle, CURLOPT_MAXFILESIZE_LARGE, transfer.limit as i64),
            curl_easy_setopt(handle, CURLOPT_ERRORBUFFER, message.as_mut_ptr()),
            curl_easy_setopt(handle, CURLOPT_WRITEFUNCTION, write as *const c_void),
            curl_easy_setopt(handle, CURLOPT_WRITEDATA, data),
            curl_easy_setopt(handle, CURLOPT_NOPROGRESS, 0 as c_long),
            curl_easy_setopt(handle, CURLOPT_XFERINFOFUNCTION, progress as *const c_void),
            curl_easy_setopt(handle, CURLOPT_XFERINFODATA, data),
        ]
    };
    let code = match options.iter().position(|&code| code != CURLE_OK) {
        Some(index) => Err(index),
        None => Ok(unsafe { curl_easy_perform(handle) }),
    };
    let mut status: c_long = 0;
    unsafe {
        curl_easy_getinfo(handle, CURLINFO_RESPONSE_CODE, &raw mut status);
        curl_easy_cleanup(handle);
        curl_slist_free_all(headers);
    }
    let code = code.map_err(|index| Error::Failed(format!("libcurl refused setting {index} of the request")))?;
    match code {
        CURLE_OK => Ok(()),
        _ if transfer.cancel.load(Ordering::Relaxed) => Err(Error::Cancelled),
        CURLE_ABORTED_BY_CALLBACK => Err(Error::Cancelled),
        CURLE_WRITE_ERROR if transfer.failure.is_some() => Err(Error::Failed(transfer.failure.take().unwrap_or_default())),
        CURLE_FILESIZE_EXCEEDED => Err(Error::Failed(format!("it is longer than the {} bytes expected", transfer.limit))),
        CURLE_HTTP_RETURNED_ERROR => Err(Error::Failed(match status {
            403 | 429 => format!("GitHub turned the request down (HTTP {status}). Without signing in it allows 60 requests an hour from one internet address; try again later"),
            404 => "GitHub has no such file (HTTP 404)".into(),
            _ => format!("GitHub answered HTTP {status}"),
        })),
        _ => {
            let text = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy();
            Err(Error::Failed(if text.is_empty() {
                unsafe { CStr::from_ptr(curl_easy_strerror(code)) }.to_string_lossy().into_owned()
            } else {
                text.into_owned()
            }))
        }
    }
}
