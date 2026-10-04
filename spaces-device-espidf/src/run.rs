//! The running module: the broker connection, and the loop that feeds the
//! protocol core and carries out what it decides.
//!
//! One thread owns the [`Module`] and everything flows to it through a
//! channel: button presses from the firmware, messages and connection changes
//! from the MQTT client. It wakes at least every [`TICK`] even when nothing
//! arrives, because a lease runs out on silence, and silence sends no message.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use esp_idf_svc::mqtt::client::{
    Details, EspMqttClient, EventPayload, MqttClientConfiguration, QoS,
};
use spaces_device::module::{Action, Config, Module, Output};
use spaces_device::wire::topic;

use crate::status_light::Status;
use crate::store::key;
use crate::update;
use crate::Online;

/// How often the loop checks the time even when nothing has happened. A lease
/// that runs out is noticed within this, so it is the slack on the cut.
pub const TICK: Duration = Duration::from_millis(50);

/// Something the module has to react to.
#[derive(Debug, Clone)]
pub enum Event {
    /// A card was presented.
    Swipe(String),
    /// Stop pressed.
    ToolOff,
    /// The machine started or stopped working.
    Running(bool),
    /// From the broker connection.
    Connected,
    Disconnected,
    Message {
        topic: String,
        payload: Vec<u8>,
    },
}

/// Drives the outputs. A relay in a real module, LEDs in the demo.
pub trait Outputs {
    fn set(&mut self, output: Output, on: bool);
}

/// Connect to the local broker and run the module until the board resets.
///
/// `events` is the receiving end of the channel; give clones of `sender` to
/// whatever reads the buttons. Never returns.
pub fn run(
    online: &Online,
    config: Config,
    mut outputs: impl Outputs,
    sender: Sender<Event>,
    events: Receiver<Event>,
) -> anyhow::Result<std::convert::Infallible> {
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
            // link drops, which is why they are made again on every connect.
            disable_clean_session: false,
            keep_alive_interval: Some(Duration::from_secs(10)),
            reconnect_timeout: Some(Duration::from_secs(3)),
            ..Default::default()
        },
        move |event| {
            let forwarded = match event.payload() {
                EventPayload::Connected(_) => Some(Event::Connected),
                EventPayload::Disconnected => Some(Event::Disconnected),
                EventPayload::Received {
                    topic: Some(topic),
                    data,
                    details: Details::Complete,
                    ..
                } => Some(Event::Message {
                    topic: topic.to_string(),
                    payload: data.to_vec(),
                }),
                EventPayload::Received { .. } => {
                    // Fragmented: far bigger than anything on the module's
                    // topics. Dropped, which for a lease reads as silence.
                    log::warn!("ignoring a message too large to arrive in one piece");
                    None
                }
                EventPayload::Error(e) => {
                    log::warn!("broker connection: {e:?}");
                    None
                }
                _ => None,
            };
            if let Some(event) = forwarded {
                let _ = sender.send(event);
            }
        },
    )?;

    let started = Instant::now();
    let now = || started.elapsed().as_millis() as u64;
    let mut module = Module::new(config);
    apply(
        module.start(now()),
        &mut client,
        &mut outputs,
        &online.updates,
    );

    loop {
        let actions = match events.recv_timeout(TICK) {
            Ok(Event::Swipe(card)) => module.swipe(&card, now()),
            Ok(Event::ToolOff) => module.tool_off(now()),
            Ok(Event::Running(on)) => module.running(on, now()),
            Ok(Event::Message { topic, payload }) => {
                // Leases and power acks arrive every second; narrate the rest.
                if topic != topic::LEASE && topic != topic::POWER_RESPONSE {
                    log::info!("<< {topic} {}", String::from_utf8_lossy(&payload));
                }
                module.message(&topic, &payload, now())
            }
            Ok(Event::Connected) => {
                log::info!("connected to the local broker; subscribing");
                online.light.set(Status::Online);
                online.updates.reach(update::BROKER);
                for name in topic::SUBSCRIPTIONS {
                    if let Err(e) = client.subscribe(name, QoS::AtMostOnce) {
                        log::warn!("could not subscribe to {name}: {e}");
                    }
                }
                module.tick(now())
            }
            Ok(Event::Disconnected) => {
                // Nothing to do on purpose: with the link gone, leases stop
                // arriving and the module switches off when the current one
                // runs out. A disconnect is not trusted to be noticed.
                log::warn!("lost the local broker; reconnecting");
                online.light.set(Status::BrokerLost);
                online.updates.lose(update::BROKER);
                module.tick(now())
            }
            Err(RecvTimeoutError::Timeout) => module.tick(now()),
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("event channel closed"),
        };
        apply(actions, &mut client, &mut outputs, &online.updates);
    }
}

fn apply(
    actions: Vec<Action>,
    client: &mut EspMqttClient<'_>,
    outputs: &mut impl Outputs,
    updates: &update::Updates,
) {
    for action in actions {
        match action {
            Action::Set { output, on } => {
                log::info!("{output:?} {}", if on { "ON" } else { "off" });
                outputs.set(output, on);
                if output == Output::Tool {
                    updates.set_in_session(on);
                }
            }
            Action::Publish { topic, payload } => {
                // enqueue, not publish: publish waits on the network, up to
                // ten seconds on a bad link, and this is the same loop that
                // has to notice a lease running out. Nothing here may block.
                if topic != topic::POWER_REPORT {
                    log::info!(">> {topic} {payload}");
                }
                if let Err(e) = client.enqueue(topic, QoS::AtMostOnce, false, payload.as_bytes()) {
                    log::warn!("could not send on {topic}: {e}");
                }
            }
            Action::Note(text) => log::info!("{text}"),
        }
    }
}
