//! Firmware updates over the network, through esp-ota-push.
//!
//! A module takes new firmware on `POST /ota`, behind a password set on the
//! setup page, and the new image is on probation until it gets back to what
//! the old one had: on the network, on the edge's broker, or with the setup
//! access point up. If it cannot, the board goes back to the old image by
//! itself. Updates are refused while a tool session is open: nobody wants a
//! laser cutter restarting mid-cut.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_ota_push::{Credentials, Device, Milestones, Options, OtaPush};

use crate::status_light::{Status, StatusLight};
use crate::store::{key, Store};

/// Joined the configured Wi-Fi network.
pub const NETWORK: Milestones = Milestones::bit(0);
/// Serving the setup access point.
pub const SETUP_AP: Milestones = Milestones::bit(1);
/// Connected to the edge's broker.
pub const BROKER: Milestones = Milestones::bit(2);

/// The username for updates. The password is the one set on the setup page.
pub const USER: &str = "admin";

/// Updates for this module. Cheap to clone; one per boot.
#[derive(Clone)]
pub struct Updates {
    /// None if OTA could not start (no OTA slots, say). The module still runs;
    /// it just takes no updates.
    ota: Option<OtaPush>,
    state: Arc<State>,
}

struct State {
    store: Store,
    light: StatusLight,
    name: &'static str,
    version: &'static str,
    /// What the module has right now, as milestones.
    now: AtomicU32,
    /// Whether a tool session is open.
    in_session: AtomicBool,
    /// What the light showed before an upload started, to put back if it is
    /// given up.
    before_upload: Mutex<Option<Status>>,
}

impl Updates {
    /// Pick up any probation left by the last update. Call early in boot.
    pub fn start(
        nvs: EspDefaultNvsPartition,
        store: Store,
        light: StatusLight,
        name: &'static str,
        version: &'static str,
    ) -> Updates {
        let ota = match OtaPush::start(nvs, Options::default()) {
            Ok(ota) => Some(ota),
            Err(e) => {
                log::warn!("no firmware updates this boot: {e}");
                None
            }
        };
        Updates {
            ota,
            state: Arc::new(State {
                store,
                light,
                name,
                version,
                now: AtomicU32::new(0),
                in_session: AtomicBool::new(false),
                before_upload: Mutex::new(None),
            }),
        }
    }

    /// The module has got somewhere: counts towards probation, and is what
    /// the next update will have to get back to.
    pub fn reach(&self, milestone: Milestones) {
        self.state.now.fetch_or(milestone.0, Ordering::Relaxed);
        if let Some(ota) = &self.ota {
            ota.reach(milestone);
        }
    }

    /// The module has lost something it had, e.g. the broker. An update
    /// arriving now will not be asked to get it back.
    pub fn lose(&self, milestone: Milestones) {
        self.state.now.fetch_and(!milestone.0, Ordering::Relaxed);
    }

    /// Someone asked for setup mode: whatever the image then fails to reach
    /// is not its fault.
    pub fn vouch(&self) {
        if let Some(ota) = &self.ota {
            ota.vouch();
        }
    }

    /// A tool session opened or closed. Updates wait for it to close.
    pub fn set_in_session(&self, open: bool) {
        self.state.in_session.store(open, Ordering::Relaxed);
    }

    /// Add the update routes, and the upload page, to `server`.
    pub fn register(&self, server: &mut EspHttpServer<'static>) -> anyhow::Result<()> {
        let Some(ota) = &self.ota else {
            return Ok(());
        };
        let device: Arc<dyn Device> = Arc::new(ModuleDevice(self.state.clone()));
        ota.register(server, device.clone())?;
        ota.register_upload_page(server, device)?;
        Ok(())
    }

    /// Serve the update routes on the network the module has joined. Nothing
    /// else is served there. Keep the server for as long as updates should be
    /// taken.
    pub fn serve(&self) -> anyhow::Result<EspHttpServer<'static>> {
        let mut server = EspHttpServer::new(&Configuration {
            // The upload handler streams through a heap buffer, but writing
            // flash goes deep enough that the default stack is too tight.
            stack_size: 10240,
            ..Default::default()
        })?;
        self.register(&mut server)?;
        Ok(server)
    }
}

struct ModuleDevice(Arc<State>);

impl Device for ModuleDevice {
    fn credentials(&self) -> Credentials {
        Credentials {
            user: USER.to_string(),
            password: self.0.store.get(key::OTA_PASS).unwrap_or_default(),
        }
    }

    fn reached(&self) -> Milestones {
        Milestones(self.0.now.load(Ordering::Relaxed))
    }

    fn name(&self) -> String {
        self.0.name.to_string()
    }

    fn version(&self) -> String {
        self.0.version.to_string()
    }

    fn may_update(&self) -> Result<(), String> {
        if self.0.in_session.load(Ordering::Relaxed) {
            Err("a tool session is open; switch the tool off first".to_string())
        } else {
            Ok(())
        }
    }

    fn progress(&self, percent: Option<u8>) {
        let mut before = self
            .0
            .before_upload
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match percent {
            Some(_) => {
                if before.is_none() {
                    *before = Some(self.0.light.get());
                    self.0.light.set(Status::Updating);
                }
            }
            None => {
                if let Some(status) = before.take() {
                    self.0.light.set(status);
                }
            }
        }
    }
}
