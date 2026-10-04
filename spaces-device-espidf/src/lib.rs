//! Runs a Spaces module on ESP-IDF.
//!
//! - [`store`]: settings and credentials in flash.
//! - [`wifi`]: joining a network, or hosting the setup one.
//! - [`portal`]: the setup page.
//! - [`register`]: claiming an invite over HTTPS.
//! - [`boot`]: putting those together in the right order.

pub mod portal;
pub mod register;
pub mod store;
pub mod wifi;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use spaces_device::registration::RegisterRequest;

use store::{key, Credentials, Field, Store};
use wifi::Wifi;

/// A module that is on the network and registered, ready to talk to its edge.
pub struct Online {
    pub wifi: Wifi,
    pub store: Store,
    pub credentials: Credentials,
    pub address: String,
}

/// Get the module online and registered, or into setup mode trying.
///
/// - No settings, or `force_setup` (e.g. BOOT held): run the setup portal, then
///   restart. Does not return.
/// - Cannot join the network: fall back to the setup portal. Does not return.
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
    force_setup: bool,
    software_version: &str,
) -> anyhow::Result<Online> {
    let store = Store::new(nvs.clone());
    let mut wifi = Wifi::new(modem, sysloop, nvs)?;
    let setup_name = format!("spaces-setup-{}", short_id(&wifi.mac()?));

    if force_setup || !store.missing(fields).is_empty() {
        if force_setup {
            log::info!("setup requested");
        } else {
            log::info!("not configured yet");
        }
        setup(&mut wifi, &store, fields, &setup_name);
    }

    let ssid = store.get(key::WIFI_SSID).unwrap_or_default();
    let pass = store.get(key::WIFI_PASS).unwrap_or_default();
    let address = match wifi.join(&ssid, &pass) {
        Ok(address) => address,
        Err(e) => {
            log::warn!("{e}; falling back to setup");
            setup(&mut wifi, &store, fields, &setup_name);
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
                anyhow::bail!(
                    "not registered and no invite saved; hold BOOT at power-on to enter one"
                );
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
    })
}

fn setup(wifi: &mut Wifi, store: &Store, fields: &'static [Field], name: &str) -> ! {
    if let Err(e) = wifi.host(name) {
        log::error!("could not start the setup access point: {e}");
    } else if let Err(e) = portal::run(store, fields, "Spaces module setup") {
        log::error!("setup portal failed: {e}");
    }
    log::info!("restarting");
    esp_idf_svc::hal::reset::restart();
}

/// The last three bytes of the MAC, enough to tell boards on one bench apart.
fn short_id(mac: &str) -> String {
    mac.split(':').skip(3).collect::<String>()
}
