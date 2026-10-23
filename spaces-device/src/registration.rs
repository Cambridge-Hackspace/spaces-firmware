//! Claiming an invite: `POST /api/devices/register`.
//!
//! The request is built and the reply read here, with no network, so the parts
//! that can be wrong are tested on the host. Sending it is the platform layer's
//! job.

use serde::{Deserialize, Serialize};

pub const PATH: &str = "/api/devices/register";

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterRequest {
    pub device_code: String,
    pub name: String,
    pub capabilities: Capabilities,
    pub mac_address: String,
    pub software_version: String,
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv4_address: Option<String>,
}

impl RegisterRequest {
    /// A request declaring exactly the roles this device can actually perform.
    ///
    /// `platform` is "other": it is not Windows, Linux or macOS.
    ///
    /// **Declare only what the hardware does.** `capabilities.roles` is the
    /// device's own claim about itself, and the server will let an
    /// administrator bind it in any role it declares. A card reader with no
    /// relay that claims `power` can therefore be bound as the thing that
    /// switches a machine, and nothing downstream can tell that it cannot --
    /// so the lie is discovered as a tool that never energizes.
    ///
    /// Declaring a role is also not the same as being given one: to receive
    /// leases the device must be bound to its tool in the `power` role by an
    /// administrator as well.
    pub fn with_roles(
        roles: &[&str],
        invite: &str,
        name: &str,
        mac_address: &str,
        software_version: &str,
        ipv4_address: Option<String>,
    ) -> Self {
        RegisterRequest {
            device_code: invite.to_string(),
            name: name.to_string(),
            capabilities: Capabilities {
                roles: roles.iter().map(|r| (*r).to_string()).collect(),
            },
            mac_address: mac_address.to_string(),
            software_version: software_version.to_string(),
            platform: "other".into(),
            ipv4_address,
        }
    }

    /// A request from a class 1 module that both reads cards and switches a
    /// tool -- the demo module's shape.
    ///
    /// Kept as the named case because it is the one a power module wants, and
    /// because it reads better at the call site than a two-element array. It is
    /// [`Self::with_roles`] with `["reader", "power"]`.
    pub fn reader_and_power(
        invite: &str,
        name: &str,
        mac_address: &str,
        software_version: &str,
        ipv4_address: Option<String>,
    ) -> Self {
        Self::with_roles(
            &["reader", "power"],
            invite,
            name,
            mac_address,
            software_version,
            ipv4_address,
        )
    }
}

/// What a successful registration yields. Persist all of it before doing
/// anything else: the invite is single-use, and a device that loses its token
/// needs a new invite, not a retry.
#[derive(Debug, Clone, PartialEq)]
pub struct Registered {
    pub device_id: String,
    pub auth_token: String,
    pub device_name: Option<String>,
    /// Kept whole, even though a module behind an edge does not use it.
    pub mqtt_config: serde_json::Value,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    data: Option<Data>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Data {
    device_id: String,
    auth_token: String,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    mqtt_config: serde_json::Value,
}

/// Read the server's reply. Anything but a clear success with both
/// identifiers is an error, with the server's own words where it gave some.
pub fn parse_reply(status: u16, body: &[u8]) -> Result<Registered, String> {
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| {
        format!(
            "HTTP {status}, and the reply is not the expected JSON: {}",
            String::from_utf8_lossy(&body[..body.len().min(200)])
        )
    })?;
    if !envelope.success || !(200..300).contains(&status) {
        let why = match envelope.error {
            Some(serde_json::Value::String(s)) => s,
            Some(other) => other.to_string(),
            None => "no reason given".into(),
        };
        return Err(format!("registration refused (HTTP {status}): {why}"));
    }
    let data = envelope
        .data
        .ok_or_else(|| "registration succeeded but returned no data".to_string())?;
    if data.device_id.is_empty() || data.auth_token.is_empty() {
        return Err("registration succeeded but returned an empty id or token".into());
    }
    Ok(Registered {
        device_id: data.device_id,
        auth_token: data.auth_token,
        device_name: data.device_name,
        mqtt_config: data.mqtt_config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_request_says_what_this_module_is() {
        let r =
            RegisterRequest::reader_and_power("INV", "demo-01", "02:00:00:00:00:01", "0.1.0", None);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["device_code"], "INV");
        assert_eq!(v["capabilities"]["roles"], json!(["reader", "power"]));
        assert_eq!(v["platform"], "other");
        assert!(v.get("ipv4_address").is_none(), "absent, not null");
    }

    #[test]
    fn a_reader_only_device_declares_only_reader() {
        // The case boot() could not express until it took `roles`: a badge
        // reader with no relay. Declaring `power` here would let an
        // administrator bind it as the thing that switches a machine, and the
        // mistake would surface as a tool that never energizes.
        let r = RegisterRequest::with_roles(
            &["reader"],
            "INV",
            "reader-01",
            "02:00:00:00:00:02",
            "0.1.0",
            None,
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["capabilities"]["roles"], json!(["reader"]));
    }

    #[test]
    fn the_named_constructor_still_declares_both() {
        // The control for the delegation: reader_and_power is now with_roles in
        // a wrapper, so this pins that the wrapper did not change what it sends.
        // Without it, a bad delegation would only be caught by whichever
        // firmware registered next -- once, on real hardware, with a spent
        // invite.
        let a = RegisterRequest::reader_and_power("INV", "d", "02:00:00:00:00:01", "0.1.0", None);
        let b = RegisterRequest::with_roles(
            &["reader", "power"],
            "INV",
            "d",
            "02:00:00:00:00:01",
            "0.1.0",
            None,
        );
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap(),
            "reader_and_power must be exactly with_roles([reader, power])"
        );
    }

    #[test]
    fn a_success_yields_the_credentials() {
        let body = json!({"success": true, "error": null, "data": {
            "device_id": "dev-1", "auth_token": "tok-1", "device_name": "demo-01",
            "mqtt_config": {"mqtt_namespace": "cs/spaces", "command_key": "ab"}}});
        let r = parse_reply(200, body.to_string().as_bytes()).unwrap();
        assert_eq!(r.device_id, "dev-1");
        assert_eq!(r.auth_token, "tok-1");
        assert_eq!(r.mqtt_config["command_key"], "ab", "kept whole");
    }

    #[test]
    fn a_null_mqtt_config_is_fine() {
        let body = json!({"success": true, "data": {
            "device_id": "dev-1", "auth_token": "tok-1", "mqtt_config": null}});
        assert!(parse_reply(200, body.to_string().as_bytes()).is_ok());
    }

    #[test]
    fn a_refusal_carries_the_servers_reason() {
        let body = json!({"success": false, "data": null, "error": "Invite already claimed"});
        let err = parse_reply(400, body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("Invite already claimed"));
        assert!(err.contains("400"));
    }

    #[test]
    fn success_false_is_a_refusal_even_with_200() {
        let body = json!({"success": false, "data": null, "error": "nope"});
        assert!(parse_reply(200, body.to_string().as_bytes()).is_err());
    }

    #[test]
    fn a_reply_without_a_token_is_not_a_success() {
        let body = json!({"success": true, "data": {"device_id": "dev-1", "auth_token": ""}});
        assert!(parse_reply(200, body.to_string().as_bytes()).is_err());
    }

    #[test]
    fn something_that_is_not_json_says_what_came_back() {
        let err = parse_reply(502, b"<html>Bad Gateway</html>").unwrap_err();
        assert!(err.contains("502") && err.contains("Bad Gateway"));
    }
}
