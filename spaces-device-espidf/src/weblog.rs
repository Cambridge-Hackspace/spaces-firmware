//! The device's log, on a web page: serial output without the cable.
//!
//! Everything the device logs, from this firmware and from ESP-IDF itself
//! (Wi-Fi, MQTT, the HTTP server), is still written to the serial port, and is
//! also kept in memory and served at `/log`, behind the device password. The
//! end of the previous boot's log is kept too, in memory that survives a
//! restart, so after a crash, a watchdog or an update that was rolled back
//! the page can still show what happened just before.
//!
//! [`start`] has to be the first thing `main` does, in place of
//! `EspLogger::initialize_default()`.

use std::ffi::{c_char, c_int};
use std::io::Write as _;
use std::sync::{LazyLock, Mutex, OnceLock};

use esp_idf_svc::http::server::{EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::{EspIOError, Write};
use esp_idf_svc::log::{EspIdfLogFilter, EspIdfLogger};
use esp_idf_svc::sys;
use esp_ota_push::core_logic::auth::{self, Access, Credentials};
use spaces_device::weblog::{strip_ansi, tail, LineAssembler, LogRing};

use crate::store::{key, Store};
use crate::update;

/// How much of this boot's log is kept: plenty for the page, small for the C6.
const RING_BYTES: usize = 16 * 1024;

/// How much of the log survives a restart. The C6 has 16 KiB of memory that
/// does; ESP-IDF uses some.
const TAIL_BYTES: usize = 3 * 1024;

/// Kept through a restart (but not a power cut): NOLOAD, so its contents are
/// whatever the last boot left. Only ever touched with STATE locked.
#[link_section = ".rtc_noinit"]
static mut TAIL: [u8; TAIL_BYTES] = [0; TAIL_BYTES];

struct State {
    ring: LogRing,
    /// ESP-IDF's output arrives in pieces; this makes lines of it.
    native: LineAssembler,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        ring: LogRing::new(RING_BYTES),
        native: LineAssembler::default(),
    })
});

/// Different on every boot, so the page can tell a restart from a quiet spell.
static BOOT_ID: OnceLock<u32> = OnceLock::new();

/// The previous boot's tail, and why it ended, read once at start.
static PREVIOUS: OnceLock<Previous> = OnceLock::new();

struct Previous {
    reason: &'static str,
    text: Option<String>,
}

/// The logger this firmware uses: ESP-IDF's, which prints to serial with its
/// per-tag levels, and a copy of every line it prints for the page.
static INNER: EspIdfLogger<EspIdfLogFilter> = EspIdfLogger::new(EspIdfLogFilter::new());
static TEE: Tee = Tee;

struct Tee;

impl log::Log for Tee {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        log::Log::enabled(&INNER, metadata)
    }

    fn log(&self, record: &log::Record) {
        if !log::Log::enabled(&INNER, record.metadata()) {
            return;
        }
        log::Log::log(&INNER, record);
        let marker = match record.level() {
            log::Level::Error => 'E',
            log::Level::Warn => 'W',
            log::Level::Info => 'I',
            log::Level::Debug => 'D',
            log::Level::Trace => 'V',
        };
        // SAFETY: reads a tick count.
        let ms = unsafe { sys::esp_log_timestamp() };
        let line = format!("{marker} ({ms}) {}: {}", record.target(), record.args());
        keep(&[line]);
    }

    fn flush(&self) {}
}

unsafe extern "C" {
    // From newlib, which ESP-IDF always links; not in the generated bindings.
    fn vsnprintf(buf: *mut c_char, size: usize, format: *const c_char, args: sys::va_list)
        -> c_int;
}

/// Where ESP-IDF's own log output is formatted. Static rather than on the
/// stack: this runs on whichever task logged, and some (the Wi-Fi driver's)
/// have little stack to spare.
static NATIVE_BUF: Mutex<[u8; 256]> = Mutex::new([0; 256]);

/// ESP-IDF's log output, all of it: printed as before, and kept.
unsafe extern "C" fn native_log(format: *const c_char, args: sys::va_list) -> c_int {
    let Ok(mut buf) = NATIVE_BUF.lock() else {
        return 0;
    };
    // SAFETY: the buffer's length is passed, and vsnprintf writes no more.
    let n = unsafe { vsnprintf(buf.as_mut_ptr().cast(), buf.len(), format, args) };
    if n < 0 {
        return n;
    }
    // A message longer than the buffer is cut, and still ends its line.
    let len = (n as usize).min(buf.len() - 1);
    let mut text = String::from_utf8_lossy(&buf[..len]).into_owned();
    drop(buf);
    if n as usize > len && !text.ends_with('\n') {
        text.push('\n');
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
    if let Ok(mut state) = STATE.lock() {
        let lines = state.native.feed(&strip_ansi(&text));
        drop(state);
        keep(&lines);
    }
    n
}

fn keep(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    for line in lines {
        state.ring.push(line);
        // SAFETY: TAIL is only touched with STATE locked, as here.
        let tail_buf = unsafe { &mut *std::ptr::addr_of_mut!(TAIL) };
        tail::append(tail_buf, line.as_bytes());
        tail::append(tail_buf, b"\n");
    }
}

/// Start logging, to serial and to the page. Call first in `main`.
pub fn start() {
    // SAFETY: nothing else runs yet, and STATE is not yet in use.
    let tail_buf = unsafe { &mut *std::ptr::addr_of_mut!(TAIL) };
    let text = tail::read(tail_buf);
    tail::reset(tail_buf);
    // SAFETY: reads why the chip last reset.
    let reason = reset_reason(unsafe { sys::esp_reset_reason() });
    let _ = PREVIOUS.set(Previous { reason, text });
    // SAFETY: reads the hardware random number generator.
    let _ = BOOT_ID.set(unsafe { sys::esp_random() });

    INNER.filter().initialize();
    if log::set_logger(&TEE).is_err() {
        // Something set a logger first; serial still works through it.
        return;
    }
    // SAFETY: installs a function with the signature ESP-IDF expects.
    unsafe {
        sys::esp_log_set_vprintf(Some(native_log));
    }
    log::info!("restarted after {reason}; the log is also at /log");
}

fn reset_reason(reason: sys::esp_reset_reason_t) -> &'static str {
    #[allow(non_upper_case_globals)]
    match reason {
        sys::esp_reset_reason_t_ESP_RST_POWERON => "power-on",
        sys::esp_reset_reason_t_ESP_RST_EXT => "an external reset",
        sys::esp_reset_reason_t_ESP_RST_SW => "a restart by the firmware",
        sys::esp_reset_reason_t_ESP_RST_PANIC => "a crash (panic)",
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => "the interrupt watchdog",
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "the task watchdog",
        sys::esp_reset_reason_t_ESP_RST_WDT => "a watchdog",
        sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "deep sleep",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "a brownout (the power dipped)",
        sys::esp_reset_reason_t_ESP_RST_USB => "a reset over USB",
        sys::esp_reset_reason_t_ESP_RST_JTAG => "a reset over JTAG",
        sys::esp_reset_reason_t_ESP_RST_CPU_LOCKUP => "a CPU lockup",
        _ => "an unknown reset",
    }
}

/// Add `/log`, `/log/lines` and `/log/previous` to `server`, behind the same
/// password as firmware updates.
pub fn register(server: &mut EspHttpServer<'static>, store: Store) -> anyhow::Result<()> {
    {
        let store = store.clone();
        server.fn_handler("/log", Method::Get, move |req| {
            if let Some(refusal) = refuse(&req, &store) {
                return refusal(req);
            }
            let mut res =
                req.into_response(200, None, &[("Content-Type", "text/html; charset=utf-8")])?;
            res.write_all(PAGE.as_bytes())?;
            Ok::<(), EspIOError>(())
        })?;
    }
    {
        let store = store.clone();
        server.fn_handler("/log/lines", Method::Get, move |req| {
            if let Some(refusal) = refuse(&req, &store) {
                return refusal(req);
            }
            let since = query_u64(req.uri(), "since").unwrap_or(0);
            let batch = STATE
                .lock()
                .map(|state| state.ring.since(since))
                .unwrap_or_else(|_| LogRing::new(0).since(0));
            let body = serde_json::json!({
                "boot": BOOT_ID.get().copied().unwrap_or(0),
                "lines": batch.lines,
                "next": batch.next,
                "missed": batch.missed,
            });
            json(req, &body)
        })?;
    }
    server.fn_handler("/log/previous", Method::Get, move |req| {
        if let Some(refusal) = refuse(&req, &store) {
            return refusal(req);
        }
        let previous = PREVIOUS.get();
        let body = serde_json::json!({
            "reason": previous.map(|p| p.reason),
            "text": previous.and_then(|p| p.text.as_deref()),
        });
        json(req, &body)
    })?;
    Ok(())
}

type Refusal = fn(Request<&mut EspHttpConnection<'_>>) -> Result<(), EspIOError>;

fn refuse(req: &Request<&mut EspHttpConnection<'_>>, store: &Store) -> Option<Refusal> {
    let credentials = Credentials {
        user: update::USER.to_string(),
        password: store.get(key::OTA_PASS).unwrap_or_default(),
    };
    match auth::check(req.header("Authorization"), &credentials) {
        Access::Allowed => None,
        Access::Denied => Some(|req| {
            let challenge = format!("Basic realm=\"{}\", charset=\"UTF-8\"", auth::REALM);
            let mut res = req.into_response(
                401,
                None,
                &[
                    ("Content-Type", "text/plain; charset=utf-8"),
                    ("WWW-Authenticate", challenge.as_str()),
                ],
            )?;
            res.write_all(b"the device password is needed to read the log\n")?;
            Ok(())
        }),
        Access::NoPassword => Some(|req| {
            let mut res =
                req.into_response(403, None, &[("Content-Type", "text/plain; charset=utf-8")])?;
            res.write_all(b"no device password has been set, so the log is not served\n")?;
            Ok(())
        }),
    }
}

fn json(
    req: Request<&mut EspHttpConnection<'_>>,
    body: &serde_json::Value,
) -> Result<(), EspIOError> {
    let mut res = req.into_response(
        200,
        None,
        &[
            ("Content-Type", "application/json"),
            ("Cache-Control", "no-store"),
        ],
    )?;
    res.write_all(body.to_string().as_bytes())?;
    Ok(())
}

/// A whole-number query parameter, e.g. `since` in `/log/lines?since=42`.
fn query_u64(uri: &str, name: &str) -> Option<u64> {
    let (_, query) = uri.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then(|| v.parse().ok()).flatten()
    })
}

const PAGE: &str = include_str!("weblog.html");
