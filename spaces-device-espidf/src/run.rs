//! The running device: the broker connection, and the loop that feeds the
//! device and carries out what it decides.
//!
//! Any [`Device`] runs here: the demo's [`spaces_device::Module`], a badge
//! reader, anything that implements the trait (#8). What a loop must get
//! right -- subscribing on every connect, never acting on a disconnect,
//! dropping fragments, holding an update for a session -- is decided by
//! [`Driver`] in `spaces-device`, where it is tested on a host (#7). This file
//! is only the glue between ESP-IDF's MQTT client and those decisions.
//!
//! One thread owns the device and everything flows to it through a channel:
//! the device's own input from other threads, messages and connection changes
//! from the MQTT client. It wakes at least every [`TICK`] even when nothing
//! arrives, because a lease runs out on silence, and silence sends no message;
//! and on every pass it lets the device's [`Hardware`] poll whatever it reads
//! on this thread.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use esp_idf_svc::mqtt::client::{
    Details, EspMqttClient, EventPayload, MqttClientConfiguration, QoS,
};
use spaces_device::{Device, Driver, Level, LinkEvent, Step};

use crate::status_light::Status;
use crate::store::key;
use crate::update;
use crate::Online;

/// How often the loop checks the time even when nothing has happened. A lease
/// that runs out is noticed within this, so it is the slack on the cut.
pub const TICK: Duration = Duration::from_millis(50);

/// Something the device has to react to.
#[derive(Debug, Clone)]
pub enum Event<I> {
    /// The device's own input, from another thread: a button, a card.
    Input(I),
    /// From the broker connection.
    Link(LinkEvent),
}

/// The device's hardware: carries out the actions that are not the loop's, and
/// reads whatever has to be read on the loop's own thread.
pub trait Hardware<D: Device> {
    /// A relay, a panel, a latch: whatever the device asked for.
    fn apply(&mut self, action: D::Action);
    /// Read inputs that live on this thread, such as a card reader on an SPI
    /// bus nothing else may touch. Called on every pass; the default reads
    /// nothing, for a device whose inputs arrive as [`Event::Input`].
    fn poll(&mut self, _now: u64) -> Vec<D::Input> {
        Vec::new()
    }
}

/// Connect to the local broker and run `device` until the board resets.
///
/// `events` is the receiving end of the channel; give clones of `sender` to
/// whatever reads the device's inputs on other threads. Never returns.
pub fn run<D, H>(
    online: &Online,
    device: D,
    mut hardware: H,
    sender: Sender<Event<D::Input>>,
    events: Receiver<Event<D::Input>>,
) -> anyhow::Result<std::convert::Infallible>
where
    D: Device,
    D::Input: Send + 'static,
    H: Hardware<D>,
{
    let broker = online.store.get(key::BROKER).unwrap_or_default();
    let username = online.store.get(key::BROKER_USER).filter(|u| !u.is_empty());
    let password = online.store.get(key::BROKER_PASS).filter(|p| !p.is_empty());
    let client_id = online.credentials.device_id.clone();

    log::info!(
        "connecting to the local broker {broker} as {}",
        username.as_deref().unwrap_or("(no login)")
    );
    let mut client = EspMqttClient::new_cb(
        &broker,
        &MqttClientConfiguration {
            client_id: Some(&client_id),
            username: username.as_deref(),
            password: password.as_deref(),
            // A clean session: the broker forgets our subscriptions when the
            // link drops, which is why the driver makes them on every connect.
            disable_clean_session: false,
            keep_alive_interval: Some(Duration::from_secs(10)),
            reconnect_timeout: Some(Duration::from_secs(3)),
            ..Default::default()
        },
        move |event| {
            let link = match event.payload() {
                EventPayload::Connected(_) => Some(LinkEvent::Connected),
                EventPayload::Disconnected => Some(LinkEvent::Disconnected),
                EventPayload::Received {
                    topic: Some(topic),
                    data,
                    details: Details::Complete,
                    ..
                } => Some(LinkEvent::Message {
                    topic: topic.to_string(),
                    payload: data.to_vec(),
                }),
                EventPayload::Received { .. } => Some(LinkEvent::Fragment),
                EventPayload::Error(e) => Some(LinkEvent::Error(format!("{e:?}"))),
                _ => None,
            };
            if let Some(link) = link {
                let _ = sender.send(Event::Link(link));
            }
        },
    )?;

    let started = Instant::now();
    let now = || started.elapsed().as_millis() as u64;
    let mut driver = Driver::new(device);
    let steps = driver.start(now());
    carry_out(steps, &mut client, &mut hardware, online);

    loop {
        let mut steps = match events.recv_timeout(TICK) {
            Ok(Event::Input(input)) => driver.input(input, now()),
            Ok(Event::Link(link)) => driver.on(link, now()),
            Err(RecvTimeoutError::Timeout) => driver.tick(now()),
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("event channel closed"),
        };
        // The Wi-Fi state comes from the subscription the Wi-Fi module already
        // holds; the driver reports it to the device only when it changes.
        steps.extend(driver.on(LinkEvent::Wifi(online.wifi.is_up()), now()));
        for input in hardware.poll(now()) {
            steps.extend(driver.input(input, now()));
        }
        carry_out(steps, &mut client, &mut hardware, online);
    }
}

fn carry_out<D: Device>(
    steps: Vec<Step<D::Action>>,
    client: &mut EspMqttClient<'_>,
    hardware: &mut impl Hardware<D>,
    online: &Online,
) {
    for step in steps {
        match step {
            Step::Subscribe(name) => {
                if let Err(e) = client.subscribe(name, QoS::AtMostOnce) {
                    log::warn!("could not subscribe to {name}: {e}");
                }
            }
            // enqueue, not publish: publish waits on the network, up to ten
            // seconds on a bad link, and this is the same loop that has to
            // notice a lease running out. Nothing here may block.
            Step::Publish { topic, payload } => {
                if let Err(e) = client.enqueue(topic, QoS::AtMostOnce, false, payload.as_bytes()) {
                    log::warn!("could not send on {topic}: {e}");
                }
            }
            Step::Log(Level::Info, text) => log::info!("{text}"),
            Step::Log(Level::Warn, text) => log::warn!("{text}"),
            Step::Session(open) => online.updates.set_in_session(open),
            Step::Broker(true) => {
                online.light.set(Status::Online);
                online.updates.reach(update::BROKER);
            }
            Step::Broker(false) => {
                online.light.set(Status::BrokerLost);
                online.updates.lose(update::BROKER);
            }
            Step::Device(action) => hardware.apply(action),
        }
    }
}
