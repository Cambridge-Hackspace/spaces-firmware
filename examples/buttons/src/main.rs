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
//! seconds at any time to get back to the setup portal.

use std::ffi::CStr;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use std::sync::mpsc;

use esp_idf_svc::hal::gpio::{Input, Output, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use spaces_device::Config;
use spaces_device_espidf::run::{Event, Outputs};
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

    let pins = peripherals.pins;
    // Hold BOOT for three seconds at any time to get back to the setup page.
    spaces_device_espidf::watch_setup_button(
        PinDriver::input(pins.gpio9, Pull::Up)?,
        store::Store::new(nvs.clone()),
    )?;

    let fields: &'static [Field] = Box::leak(
        store::FIELDS
            .iter()
            .chain(DEMO_FIELDS)
            .copied()
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );

    let online = match spaces_device_espidf::boot(peripherals.modem, sysloop, nvs, fields, version)
    {
        Ok(online) => online,
        Err(e) => {
            log::error!("{e:#}");
            log::error!("restarting in 30 s");
            std::thread::sleep(Duration::from_secs(30));
            esp_idf_svc::hal::reset::restart();
        }
    };

    let alice = online.store.get(ALICE_CARD).unwrap_or_default();
    let bob = online.store.get(BOB_CARD).unwrap_or_default();
    let tool_id = online.store.get(store::key::TOOL_ID).unwrap_or_default();
    log::info!(
        "online at {} as device {}; tool {tool_id:?}, Alice is {alice:?}, Bob is {bob:?}",
        online.address,
        online.credentials.device_id,
    );

    let outputs = Leds {
        tool: PinDriver::output(pins.gpio22)?,
        running: PinDriver::output(pins.gpio23)?,
    };
    let (sender, events) = mpsc::channel();

    let buttons = [
        (
            PinDriver::input(pins.gpio18, Pull::Up)?,
            Event::Swipe(alice),
        ),
        (PinDriver::input(pins.gpio19, Pull::Up)?, Event::Swipe(bob)),
        (PinDriver::input(pins.gpio20, Pull::Up)?, Event::ToolOff),
    ];
    let running = PinDriver::input(pins.gpio21, Pull::Up)?;
    let button_sender = sender.clone();
    std::thread::Builder::new()
        .stack_size(4096)
        .spawn(move || read_inputs(buttons, running, button_sender))?;

    let config = Config::new(online.credentials.device_id.clone(), tool_id);
    match spaces_device_espidf::run::run(&online, config, outputs, sender, events) {
        Err(e) => log::error!("{e:#}"),
        Ok(never) => match never {},
    }
    log::error!("restarting in 5 s");
    std::thread::sleep(Duration::from_secs(5));
    esp_idf_svc::hal::reset::restart();
}

/// The two LEDs, each through a resistor to ground.
struct Leds {
    tool: PinDriver<'static, Output>,
    running: PinDriver<'static, Output>,
}

impl Outputs for Leds {
    fn set(&mut self, output: spaces_device::Output, on: bool) {
        let pin = match output {
            spaces_device::Output::Tool => &mut self.tool,
            spaces_device::Output::Running => &mut self.running,
        };
        let _ = if on { pin.set_high() } else { pin.set_low() };
    }
}

/// Watch the buttons and the running switch, and report changes. A button
/// counts once per press; the switch reports both directions. Pins read low
/// when pressed, because they are pulled up and wired to ground.
fn read_inputs(
    buttons: [(PinDriver<'static, Input>, Event); 3],
    running: PinDriver<'static, Input>,
    sender: mpsc::Sender<Event>,
) {
    let mut pressed = [false; 3];
    let mut was_running = false;
    let mut steady = [0u8; 4];
    loop {
        for (i, (pin, event)) in buttons.iter().enumerate() {
            let now = pin.is_low();
            // Debounce: act only once the pin has read the new state for
            // three samples running (30 ms).
            if now != pressed[i] {
                steady[i] += 1;
                if steady[i] >= 3 {
                    pressed[i] = now;
                    steady[i] = 0;
                    if now {
                        let _ = sender.send(event.clone());
                    }
                }
            } else {
                steady[i] = 0;
            }
        }
        let now = running.is_low();
        if now != was_running {
            steady[3] += 1;
            if steady[3] >= 3 {
                was_running = now;
                steady[3] = 0;
                let _ = sender.send(Event::Running(now));
            }
        } else {
            steady[3] = 0;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
