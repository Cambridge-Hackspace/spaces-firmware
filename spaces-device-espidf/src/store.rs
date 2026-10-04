//! What a module keeps in flash: the settings typed into the setup portal, and
//! the credentials it was given at registration.
//!
//! The credentials matter most. The invite a module registers with is
//! single-use, so a module that loses its token cannot get it back by trying
//! again: it needs a new invite from an administrator. They are written to
//! flash before anything else is done with them.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use serde::{Deserialize, Serialize};
pub use spaces_device::setup_page::Field;

const SETTINGS: &str = "spaces_set";
const CREDENTIALS: &str = "spaces_cred";

/// NVS keys are capped at 15 characters, so every key used is listed here.
pub mod key {
    pub const WIFI_SSID: &str = "wifi_ssid";
    pub const WIFI_PASS: &str = "wifi_pass";
    pub const SERVER: &str = "server";
    pub const INVITE: &str = "invite";
    pub const BROKER: &str = "broker";
    pub const TOOL_ID: &str = "tool_id";
    pub const NAME: &str = "name";
}

/// The settings every module needs. A particular firmware adds its own (the
/// demo adds the cards its buttons send) and passes both to the portal.
pub const FIELDS: &[Field] = &[
    Field {
        key: key::WIFI_SSID,
        label: "Wi-Fi network",
        hint: "",
        secret: false,
        required: true,
    },
    Field {
        key: key::WIFI_PASS,
        label: "Wi-Fi password",
        hint: "leave blank to keep the saved one",
        secret: true,
        required: false,
    },
    Field {
        key: key::SERVER,
        label: "Spaces server",
        hint: "e.g. https://spaces.example.org; used once, to register",
        secret: false,
        required: true,
    },
    Field {
        key: key::INVITE,
        label: "Device invite",
        // Shown in the clear on purpose: invites are emoji, meant to be pasted
        // from the admin page's copy button, and a password field would hide
        // a paste that went wrong. Single-use and deleted once claimed, so
        // there is nothing to protect by masking it.
        hint: "paste it from /admin/devices; single-use, deleted once claimed",
        secret: false,
        required: false,
    },
    Field {
        key: key::BROKER,
        label: "Local broker",
        hint: "the edge's MQTT broker, e.g. mqtt://192.168.1.20:1883",
        secret: false,
        required: true,
    },
    Field {
        key: key::TOOL_ID,
        label: "Tool",
        hint: "the machine this module controls: its external id on the platform, e.g. laser-01",
        secret: false,
        required: true,
    },
    Field {
        key: key::NAME,
        label: "Device name",
        hint: "a label for this board in the device list, e.g. demo-c6-01; for humans only",
        secret: false,
        required: true,
    },
];

/// What registration hands back, kept exactly as received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub device_id: String,
    pub auth_token: String,
    /// The whole `mqtt_config` object, which may be null. A module behind an
    /// edge does not use the site broker, but FIRMWARE.md says to keep it.
    #[serde(default)]
    pub mqtt_config: serde_json::Value,
}

#[derive(Clone)]
pub struct Store {
    partition: EspDefaultNvsPartition,
}

impl Store {
    pub fn new(partition: EspDefaultNvsPartition) -> Self {
        Store { partition }
    }

    fn open(&self, namespace: &str) -> anyhow::Result<EspNvs<NvsDefault>> {
        Ok(EspNvs::new(self.partition.clone(), namespace, true)?)
    }

    /// A setting, or `None` if it was never set.
    pub fn get(&self, key: &str) -> Option<String> {
        let nvs = self.open(SETTINGS).ok()?;
        let len = nvs.str_len(key).ok()??;
        let mut buf = vec![0u8; len];
        nvs.get_str(key, &mut buf).ok()?.map(str::to_string)
    }

    pub fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.open(SETTINGS)?.set_str(key, value)?;
        Ok(())
    }

    pub fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.open(SETTINGS)?.remove(key)?;
        Ok(())
    }

    /// Every required field that has no value yet.
    pub fn missing<'a>(&self, fields: &'a [Field]) -> Vec<&'a Field> {
        fields
            .iter()
            .filter(|f| f.required && self.get(f.key).is_none_or(|v| v.trim().is_empty()))
            .collect()
    }

    pub fn credentials(&self) -> Option<Credentials> {
        let nvs = self.open(CREDENTIALS).ok()?;
        let len = nvs.str_len("creds").ok()??;
        let mut buf = vec![0u8; len];
        let json = nvs.get_str("creds", &mut buf).ok()??;
        serde_json::from_str(json).ok()
    }

    pub fn save_credentials(&self, credentials: &Credentials) -> anyhow::Result<()> {
        let json = serde_json::to_string(credentials)?;
        self.open(CREDENTIALS)?.set_str("creds", &json)?;
        Ok(())
    }
}
