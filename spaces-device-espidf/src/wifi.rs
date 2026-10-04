//! Joining a network, and hosting one for setup.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::{EspSubscription, EspSystemEventLoop, System};
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi, WifiEvent,
};
use spaces_device::backoff::Backoff;

/// The address of the setup portal while the module is its own access point.
/// This is esp-idf's default for an access point.
pub const SETUP_ADDRESS: &str = "192.168.71.1";

/// How long one attempt to join may take, scan included. Most failures end an
/// attempt well before this, with a disconnect: about 2.5 s for a network
/// that is not there, about 4 s for one that refuses us.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Wifi {
    inner: BlockingWifi<EspWifi<'static>>,
    /// Set once joined: from then on a lost connection is rejoined at once.
    rejoin: Arc<AtomicBool>,
    /// Disconnects so far, and the reason given for the latest: how an
    /// attempt that has already failed finds out without waiting it out.
    disconnects: Arc<AtomicU32>,
    last_reason: Arc<AtomicU32>,
    _disconnects: EspSubscription<'static, System>,
}

impl Wifi {
    pub fn new(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
    ) -> anyhow::Result<Self> {
        let wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        let rejoin = Arc::new(AtomicBool::new(false));
        let disconnects = Arc::new(AtomicU32::new(0));
        let last_reason = Arc::new(AtomicU32::new(0));
        let subscription = {
            let rejoin = rejoin.clone();
            let disconnects = disconnects.clone();
            let last_reason = last_reason.clone();
            sysloop.subscribe::<WifiEvent, _>(move |event| {
                if let WifiEvent::StaDisconnected(why) = event {
                    // The 802.11 reason code is the only clue to why a join
                    // failed or a link dropped, so it is always logged.
                    log::warn!("Wi-Fi disconnected, reason {}", why.reason());
                    last_reason.store(why.reason().into(), Ordering::Relaxed);
                    disconnects.fetch_add(1, Ordering::Release);
                    if rejoin.load(Ordering::Relaxed) {
                        // What esp-idf's own examples do. If the network is
                        // gone this fails after a scan of a few seconds and
                        // lands back here, so it retries without spinning.
                        // SAFETY: the driver is started; this only queues a
                        // request with it.
                        let err = unsafe { esp_idf_svc::sys::esp_wifi_connect() };
                        if err != 0 {
                            log::warn!("could not ask to rejoin: error {err}");
                        }
                    }
                }
            })?
        };
        Ok(Wifi {
            inner: BlockingWifi::wrap(wifi, sysloop)?,
            rejoin,
            disconnects,
            last_reason,
            _disconnects: subscription,
        })
    }

    /// Join `ssid`, trying for as long as it takes, and return the address we
    /// were given. Once joined, a dropped connection is rejoined
    /// automatically for as long as the module runs.
    ///
    /// Never gives up: see [`Backoff`] for why. Setup mode stays reachable
    /// throughout by holding BOOT. Errors only if the driver itself fails.
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

        let mut failures = 0;
        loop {
            self.inner.start()?;
            log::info!("joining {ssid} (attempt {})", failures + 1);
            match self.try_once() {
                Ok(()) => {
                    let ip = self.inner.wifi().sta_netif().get_ip_info()?.ip;
                    log::info!("joined {ssid} as {ip}");
                    self.rejoin.store(true, Ordering::Relaxed);
                    return Ok(ip.to_string());
                }
                Err(e) => log::warn!("attempt {} failed: {e}", failures + 1),
            }
            failures += 1;
            // Stopped, not just disconnected. Seen on the bench: a connect
            // issued after a failed attempt, even two seconds after a
            // disconnect, sat for the whole timeout without scanning or
            // authenticating. Restarting the driver clears whatever it was
            // stuck on.
            let _ = self.inner.stop();
            let wait = Backoff::WIFI.after(failures);
            log::info!("trying again in {} s", wait.as_secs());
            std::thread::sleep(wait);
        }
    }

    fn try_once(&mut self) -> anyhow::Result<()> {
        let deadline = Instant::now() + ATTEMPT_TIMEOUT;
        // Any disconnect from here on is this attempt failing. The driver
        // does not try again by itself, so there is nothing to wait for.
        let before = self.disconnects.load(Ordering::Acquire);
        self.inner.wifi_mut().connect()?;
        while !self.inner.is_connected()? {
            if self.disconnects.load(Ordering::Acquire) != before {
                anyhow::bail!(
                    "disconnected, reason {}",
                    self.last_reason.load(Ordering::Relaxed)
                );
            }
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
        self.rejoin.store(false, Ordering::Relaxed);
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
