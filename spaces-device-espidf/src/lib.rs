//! Runs a Spaces module on ESP-IDF.
//!
//! - [`status_light`]: the RGB LED that shows what the module is doing.
//! - [`store`]: settings and credentials in flash.
//! - [`wifi`]: joining a network, or hosting the setup one.
//! - [`portal`]: the setup page.
//! - [`register`]: claiming an invite over HTTPS.
//! - [`run`]: the broker connection and the loop that drives the protocol.
//! - [`update`]: taking new firmware over the network, with rollback.
//! - [`boot`]: putting those together in the right order.

pub mod portal;
pub mod register;
pub mod run;
pub mod status_light;
pub mod store;
pub mod update;
pub mod wifi;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use spaces_device::registration::RegisterRequest;

use status_light::{Status, StatusLight};
use store::{key, Credentials, Field, Store};
use update::Updates;
use wifi::Wifi;

/// A module that is on the network and registered, ready to talk to its edge.
pub struct Online {
    pub wifi: Wifi,
    pub store: Store,
    pub credentials: Credentials,
    pub address: String,
    pub light: StatusLight,
    pub updates: Updates,
    /// Serves firmware updates on the network, and nothing else. Kept here so
    /// it lives as long as the module does.
    pub update_server: Option<esp_idf_svc::http::server::EspHttpServer<'static>>,
}

/// Watch a button (BOOT, on the demo) and, once it has been held for three
/// seconds, restart into setup mode.
///
/// Watched while running, not checked at power-on: on an ESP32, holding BOOT
/// while the chip starts puts it into its download mode, so "hold BOOT at
/// power-on" would never reach the firmware at all.
pub fn watch_setup_button(
    button: esp_idf_svc::hal::gpio::PinDriver<'static, esp_idf_svc::hal::gpio::Input>,
    store: Store,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .stack_size(4096)
        .spawn(move || {
            let mut held_ms = 0u32;
            loop {
                if button.is_low() {
                    held_ms += 100;
                    if held_ms >= 3000 {
                        log::info!("BOOT held for 3 s: restarting into setup");
                        let _ = store.set(key::SETUP_REQUESTED, "1");
                        esp_idf_svc::hal::reset::restart();
                    }
                } else {
                    held_ms = 0;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })?;
    Ok(())
}

/// Get the module online and registered, or into setup mode trying.
///
/// - No settings, or setup requested with [`watch_setup_button`]: run the
///   setup portal, then restart. Does not return.
/// - Cannot join the network: keep trying, for as long as it takes. Holding
///   BOOT still reaches setup meanwhile.
/// - Not yet registered: claim the invite, and save what comes back before
///   anything else. If that fails the module cannot work, and says so.
///
/// Deliberately makes no other HTTP calls. A module behind an edge leaves
/// boot-reset, module-state and power-state to the edge: boot-reset in
/// particular is global, and a module calling it would end every session in
/// the building whenever it restarted.
pub fn boot(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    fields: &'static [Field],
    software_version: &str,
    light: StatusLight,
    updates: Updates,
) -> anyhow::Result<Online> {
    let store = Store::new(nvs.clone());
    let force_setup = store.get(key::SETUP_REQUESTED).is_some();
    if force_setup {
        store.remove(key::SETUP_REQUESTED)?;
        // A person chose setup mode; that is not the new image failing.
        updates.vouch();
    }
    let mut wifi = Wifi::new(modem, sysloop, nvs)?;
    let setup_name = format!("spaces-setup-{}", short_id(&wifi.mac()?));

    if force_setup || !store.missing(fields).is_empty() {
        if force_setup {
            log::info!("setup requested");
        } else {
            log::info!("not configured yet");
        }
        light.set(Status::Setup);
        setup(&mut wifi, &store, fields, &setup_name, &updates);
    }

    light.set(Status::Connecting);
    let ssid = store.get(key::WIFI_SSID).unwrap_or_default();
    let pass = store.get(key::WIFI_PASS).unwrap_or_default();
    // Tries until it joins. Falling back to setup mode here would open an
    // unsecured access point to anyone in range whenever the network went
    // away for long enough; BOOT is the way into setup instead.
    let address = wifi.join(&ssid, &pass)?;
    updates.reach(update::NETWORK);
    let update_server = match updates.serve() {
        Ok(server) => Some(server),
        Err(e) => {
            log::warn!("not taking firmware updates: {e}");
            None
        }
    };

    let credentials = match store.credentials() {
        Some(c) => {
            log::info!("registered as {}", c.device_id);
            c
        }
        None => {
            let invite = store.get(key::INVITE).unwrap_or_default();
            if invite.trim().is_empty() {
                anyhow::bail!("not registered and no invite saved; hold BOOT for 3 s to enter one");
            }
            let server = store.get(key::SERVER).unwrap_or_default();
            let name = store.get(key::NAME).unwrap_or_else(|| setup_name.clone());
            let request = RegisterRequest::reader_and_power(
                invite.trim(),
                &name,
                &wifi.mac()?,
                software_version,
                Some(address.clone()),
            );
            let registered = register::register(&server, &request)?;
            let credentials = Credentials {
                device_id: registered.device_id,
                auth_token: registered.auth_token,
                mqtt_config: registered.mqtt_config,
            };
            // Before anything else: the invite cannot be used again.
            store.save_credentials(&credentials)?;
            store.remove(key::INVITE)?;
            log::info!("registered as {}; credentials saved", credentials.device_id);
            credentials
        }
    };

    Ok(Online {
        wifi,
        store,
        credentials,
        address,
        light,
        updates,
        update_server,
    })
}

fn setup(
    wifi: &mut Wifi,
    store: &Store,
    fields: &'static [Field],
    name: &str,
    updates: &Updates,
) -> ! {
    if let Err(e) = wifi.host(name) {
        log::error!("could not start the setup access point: {e}");
    } else if let Err(e) = {
        updates.reach(update::SETUP_AP);
        portal::run(store, fields, "Spaces module setup", updates)
    } {
        log::error!("setup portal failed: {e}");
    }
    log::info!("restarting");
    esp_idf_svc::hal::reset::restart();
}

/// The last three bytes of the MAC, enough to tell boards on one bench apart.
fn short_id(mac: &str) -> String {
    mac.split(':').skip(3).collect::<String>()
}
