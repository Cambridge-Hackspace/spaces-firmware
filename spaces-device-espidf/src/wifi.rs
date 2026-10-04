//! Joining a network, and hosting one for setup.

use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};

/// The address of the setup portal while the module is its own access point.
/// This is esp-idf's default for an access point.
pub const SETUP_ADDRESS: &str = "192.168.71.1";

/// Association attempts before giving up and falling back to setup mode. One
/// attempt is not enough evidence: a single transient auth timeout used to
/// strand the traffic light in setup mode until someone power-cycled it.
const ATTEMPTS: u32 = 3;
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(2);

pub struct Wifi {
    inner: BlockingWifi<EspWifi<'static>>,
}

impl Wifi {
    pub fn new(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
    ) -> anyhow::Result<Self> {
        let wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        Ok(Wifi {
            inner: BlockingWifi::wrap(wifi, sysloop)?,
        })
    }

    /// Join `ssid`. Returns the address we were given, or an error once every
    /// attempt has failed.
    pub fn join(&mut self, ssid: &str, password: &str) -> anyhow::Result<String> {
        let config = ClientConfiguration {
            ssid: ssid
                .try_into()
                .map_err(|_| anyhow::anyhow!("network name is too long"))?,
            password: password
                .try_into()
                .map_err(|_| anyhow::anyhow!("password is too long"))?,
            auth_method: if password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            ..Default::default()
        };
        self.inner
            .set_configuration(&Configuration::Client(config))?;
        self.inner.start()?;

        for attempt in 1..=ATTEMPTS {
            log::info!("joining {ssid} (attempt {attempt}/{ATTEMPTS})");
            match self.try_once() {
                Ok(()) => {
                    let ip = self.inner.wifi().sta_netif().get_ip_info()?.ip;
                    log::info!("joined {ssid} as {ip}");
                    return Ok(ip.to_string());
                }
                Err(e) => log::warn!("attempt {attempt} failed: {e}"),
            }
            let _ = self.inner.disconnect();
            // Seen on the bench: a connect issued straight after a failed
            // attempt can sit for the whole timeout without even trying to
            // authenticate. Give the driver a moment to settle first.
            std::thread::sleep(SETTLE);
        }
        anyhow::bail!("could not join {ssid} after {ATTEMPTS} attempts")
    }

    fn try_once(&mut self) -> anyhow::Result<()> {
        let deadline = Instant::now() + ATTEMPT_TIMEOUT;
        self.inner.wifi_mut().connect()?;
        while !self.inner.is_connected()? {
            if Instant::now() > deadline {
                anyhow::bail!("timed out");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.inner.wait_netif_up()?;
        Ok(())
    }

    /// Become an open access point called `name`, for the setup portal.
    pub fn host(&mut self, name: &str) -> anyhow::Result<()> {
        let config = AccessPointConfiguration {
            ssid: name
                .try_into()
                .map_err(|_| anyhow::anyhow!("access point name is too long"))?,
            auth_method: AuthMethod::None,
            channel: 1,
            ..Default::default()
        };
        self.inner
            .set_configuration(&Configuration::AccessPoint(config))?;
        self.inner.start()?;
        self.inner.wait_netif_up()?;
        log::info!("setup access point {name} is up at http://{SETUP_ADDRESS}/");
        Ok(())
    }

    /// The radio's MAC address, as registration wants it.
    pub fn mac(&self) -> anyhow::Result<String> {
        let mac = self.inner.wifi().sta_netif().get_mac()?;
        Ok(mac
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"))
    }
}
