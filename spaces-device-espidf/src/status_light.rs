//! The status light: one WS2812 RGB LED, showing [`Status`] in the patterns
//! [`spaces_device::status`] defines.
//!
//! A thread of its own owns the LED and redraws it every [`FRAME`]; everything
//! else just sets the status through a [`StatusLight`], which is cheap to clone
//! and never blocks. A board without the LED, or one whose LED failed to start,
//! gets a light that ignores what it is told: a status light is never a reason
//! for a module not to run.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::gpio::OutputPin;
use esp_idf_svc::hal::rmt::config::{TransmitConfig, TxChannelConfig};
use esp_idf_svc::hal::rmt::encoder::{BytesEncoder, BytesEncoderConfig};
use esp_idf_svc::hal::rmt::{PinState, Symbol, TxChannelDriver};
use esp_idf_svc::hal::units::Hertz;
pub use spaces_device::status::Status;
use spaces_device::status::{colour, Rgb};

/// How often the light is redrawn: fast enough that the setup pulse is smooth.
const FRAME: Duration = Duration::from_millis(20);

/// RMT tick rate: 10 MHz, so one tick is 0.1 µs.
const RESOLUTION: Hertz = Hertz(10_000_000);

/// WS2812 bit timings, from its datasheet. The LED latches a colour once its
/// data line has been low for more than 50 µs, which the gap between frames
/// gives it for free.
const T0H: Duration = Duration::from_nanos(350);
const T0L: Duration = Duration::from_nanos(800);
const T1H: Duration = Duration::from_nanos(700);
const T1L: Duration = Duration::from_nanos(600);

#[derive(Clone)]
pub struct StatusLight {
    status: Option<Arc<AtomicU8>>,
}

impl StatusLight {
    /// Start driving a WS2812 on `pin`, showing `initial`. If the LED cannot be
    /// started, says so in the log and returns a light that does nothing.
    pub fn start(pin: impl OutputPin + Send + 'static, initial: Status) -> StatusLight {
        match Self::try_start(pin, initial) {
            Ok(light) => light,
            Err(e) => {
                log::warn!("no status light: {e}");
                StatusLight::none()
            }
        }
    }

    /// A light that ignores what it is told, for boards without one.
    pub fn none() -> StatusLight {
        StatusLight { status: None }
    }

    /// What it is showing; Online for a light that is not there.
    pub fn get(&self) -> Status {
        self.status
            .as_ref()
            .and_then(|shared| Status::from_u8(shared.load(Ordering::Relaxed)))
            .unwrap_or(Status::Online)
    }

    pub fn set(&self, status: Status) {
        if let Some(shared) = &self.status {
            shared.store(status as u8, Ordering::Relaxed);
        }
    }

    fn try_start(
        pin: impl OutputPin + Send + 'static,
        initial: Status,
    ) -> anyhow::Result<StatusLight> {
        let shared = Arc::new(AtomicU8::new(initial as u8));
        let status = shared.clone();
        // The encoder cannot move between threads, so the LED is set up on
        // the thread that drives it, and says back whether that worked.
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .stack_size(4096)
            .spawn(move || {
                let (mut channel, mut encoder) = match open(pin) {
                    Ok(led) => {
                        let _ = started_tx.send(Ok(()));
                        led
                    }
                    Err(e) => {
                        let _ = started_tx.send(Err(e));
                        return;
                    }
                };
                let started = Instant::now();
                let mut shown = None;
                loop {
                    let status =
                        Status::from_u8(status.load(Ordering::Relaxed)).unwrap_or(Status::Failed);
                    let now = started.elapsed().as_millis() as u64;
                    let want = colour(status, now);
                    // Only send when it changes: most of the time it does not.
                    if shown != Some(want) {
                        match show(&mut channel, &mut encoder, want) {
                            Ok(()) => shown = Some(want),
                            Err(e) => log::warn!("status light: {e}"),
                        }
                    }
                    std::thread::sleep(FRAME);
                }
            })?;
        started_rx.recv()??;
        Ok(StatusLight {
            status: Some(shared),
        })
    }
}

fn open(pin: impl OutputPin + 'static) -> anyhow::Result<(TxChannelDriver<'static>, BytesEncoder)> {
    let channel = TxChannelDriver::new(
        pin,
        &TxChannelConfig {
            resolution: RESOLUTION,
            ..Default::default()
        },
    )?;
    let encoder = BytesEncoder::with_config(&BytesEncoderConfig {
        bit0: Symbol::new_with(RESOLUTION, PinState::High, T0H, PinState::Low, T0L)?,
        bit1: Symbol::new_with(RESOLUTION, PinState::High, T1H, PinState::Low, T1L)?,
        msb_first: true,
        ..Default::default()
    })?;
    Ok((channel, encoder))
}

fn show(
    channel: &mut TxChannelDriver<'_>,
    encoder: &mut BytesEncoder,
    c: Rgb,
) -> anyhow::Result<()> {
    // A WS2812 takes its colour green first.
    channel.send_and_wait(encoder, &[c.g, c.r, c.b], &TransmitConfig::default())?;
    Ok(())
}
