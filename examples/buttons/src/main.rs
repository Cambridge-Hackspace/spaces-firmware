//! The Spaces demo module.
//!
//! An ESP32-C6 with buttons standing in for people swiping cards and LEDs
//! standing in for a machine:
//!
//! | GPIO | Part | Meaning |
//! |---|---|---|
//! | 18 | button | Alice swipes her card |
//! | 19 | button | Bob swipes his |
//! | 20 | button | Tool off |
//! | 21 | switch | The machine is running (e.g. a laser firing) |
//! | 22 | LED | The tool is powered: authorized *and* leased |
//! | 23 | LED | The tool is running: powered *and* the switch is on |
//!
//! Buttons and the switch connect their pin to GND. Hold BOOT for three
//! seconds at power-on to get back to the setup portal.

use std::ffi::CStr;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{Input, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use spaces_device_espidf::store::{self, Field};

/// The demo's own settings, on top of the ones every module needs.
const ALICE_CARD: &str = "alice_card";
const BOB_CARD: &str = "bob_card";

const DEMO_FIELDS: &[Field] = &[
    Field {
        key: ALICE_CARD,
        label: "Alice's card",
        hint: "a card authorized on the tool, sent when the Alice button is pressed",
        secret: false,
        required: true,
    },
    Field {
        key: BOB_CARD,
        label: "Bob's card",
        hint: "a card that should be refused, sent when the Bob button is pressed",
        secret: false,
        required: true,
    },
];

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // esp-idf-svc logs every storage handle it closes, which buries the
    // narration this demo exists to show. Its logger follows ESP-IDF's
    // per-tag levels, and a Rust target is its tag.
    unsafe {
        esp_idf_svc::sys::esp_log_level_set(
            c"esp_idf_svc::nvs".as_ptr(),
            esp_idf_svc::sys::esp_log_level_t_ESP_LOG_WARN,
        );
    }

    let version = env!("CARGO_PKG_VERSION");
    log::info!(
        "spaces-firmware buttons demo {version}, running from {}",
        running_slot()
    );

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let boot = PinDriver::input(peripherals.pins.gpio9, Pull::Up)?;
    let force_setup = held_for(&boot, Duration::from_secs(3));
    drop(boot);

    let fields: &'static [Field] = Box::leak(
        store::FIELDS
            .iter()
            .chain(DEMO_FIELDS)
            .copied()
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );

    let online = match spaces_device_espidf::boot(
        peripherals.modem,
        sysloop,
        nvs,
        fields,
        force_setup,
        version,
    ) {
        Ok(online) => online,
        Err(e) => {
            log::error!("{e:#}");
            log::error!("restarting in 30 s");
            std::thread::sleep(Duration::from_secs(30));
            esp_idf_svc::hal::reset::restart();
        }
    };

    log::info!(
        "online at {} as device {}; Alice is {:?}, Bob is {:?}",
        online.address,
        online.credentials.device_id,
        online.store.get(ALICE_CARD).unwrap_or_default(),
        online.store.get(BOB_CARD).unwrap_or_default(),
    );

    // The protocol itself comes in the next step.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Whether a button pulled to ground stays pressed for `hold`.
fn held_for(pin: &PinDriver<'_, Input>, hold: Duration) -> bool {
    let steps = hold.as_millis() / 100;
    for _ in 0..steps {
        if pin.is_high() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Which app slot this image booted from.
fn running_slot() -> String {
    let part = unsafe { esp_idf_svc::sys::esp_ota_get_running_partition() };
    if part.is_null() {
        return "an unknown partition".to_string();
    }
    unsafe { CStr::from_ptr((*part).label.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
