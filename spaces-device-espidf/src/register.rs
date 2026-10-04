//! Sending the registration request over HTTPS.

use embedded_svc::http::client::Client;
use embedded_svc::http::Method;
use embedded_svc::io::Write;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use spaces_device::registration::{self, RegisterRequest, Registered};

/// Claim the invite. The server URL is used as given, e.g.
/// `https://spaces.example.org`; certificates are checked against the bundle
/// of public CAs that ships with esp-idf.
pub fn register(server: &str, request: &RegisterRequest) -> anyhow::Result<Registered> {
    let url = format!("{}{}", server.trim_end_matches('/'), registration::PATH);
    let body = serde_json::to_vec(request)?;

    let connection = EspHttpConnection::new(&Configuration {
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(std::time::Duration::from_secs(20)),
        ..Default::default()
    })?;
    let mut client = Client::wrap(connection);

    let length = body.len().to_string();
    let headers = [
        ("Content-Type", "application/json"),
        ("Content-Length", length.as_str()),
    ];
    log::info!("registering with {url}");
    let mut req = client.request(Method::Post, &url, &headers)?;
    req.write_all(&body)?;
    req.flush()?;
    let mut res = req.submit()?;
    let status = res.status();

    let mut reply = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = res.read(&mut buf)?;
        if n == 0 || reply.len() > 16 * 1024 {
            break;
        }
        reply.extend_from_slice(&buf[..n]);
    }
    registration::parse_reply(status, &reply).map_err(|e| anyhow::anyhow!(e))
}
