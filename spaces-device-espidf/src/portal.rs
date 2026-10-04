//! The setup portal: a single page, served while the module is its own access
//! point, where its settings are typed in.
//!
//! It only ever runs in setup mode, on a network with nobody else on it. Once
//! the module is on the real network there is no settings page at all; that is
//! a choice, to keep the demo about the protocol.

use std::sync::mpsc;

use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;

use spaces_device::form::parse_form;
use spaces_device::setup_page;

use crate::store::{Field, Store};
use crate::update::Updates;

/// The encoding goes in the header as well as the page: a browser trusts the
/// header first, and an invite made of emoji depends on it.
const HTML: &[(&str, &str)] = &[("Content-Type", "text/html; charset=utf-8")];

/// Serve the portal until the settings are saved. Returns when the page has
/// told the browser it is done; the caller then restarts.
/// Firmware updates are served alongside, so a module stuck in setup can
/// still be given new firmware over its own access point.
pub fn run(
    store: &Store,
    fields: &'static [Field],
    title: &str,
    updates: &Updates,
) -> anyhow::Result<()> {
    let (saved_tx, saved_rx) = mpsc::channel::<()>();
    let mut server = EspHttpServer::new(&Configuration {
        stack_size: 10240,
        ..Default::default()
    })?;
    updates.register(&mut server)?;
    crate::weblog::register(&mut server, store.clone())?;

    {
        let store = store.clone();
        let title = title.to_string();
        server.fn_handler("/", Method::Get, move |req| {
            let page = page(&store, fields, &title, None);
            req.into_response(200, None, HTML)?
                .write_all(page.as_bytes())?;
            Ok::<(), anyhow::Error>(())
        })?;
    }

    {
        let store = store.clone();
        let title = title.to_string();
        server.fn_handler("/save", Method::Post, move |mut req| {
            let mut body = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let n = req.read(&mut buf)?;
                if n == 0 || body.len() > 8192 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
            }
            let form = parse_form(&String::from_utf8_lossy(&body));
            for field in fields {
                let value = form.get(field.key).map(|v| v.trim()).unwrap_or("");
                // A blank secret means "keep the one already saved", so the page
                // never has to show it.
                if field.secret && value.is_empty() {
                    continue;
                }
                store.set(field.key, value)?;
            }
            let missing = store.missing(fields);
            if missing.is_empty() {
                let done = setup_page::render_saved(&title);
                req.into_response(200, None, HTML)?
                    .write_all(done.as_bytes())?;
                let _ = saved_tx.send(());
            } else {
                let names: Vec<_> = missing.iter().map(|f| f.label).collect();
                let note = format!("Still needed: {}", names.join(", "));
                let page = page(&store, fields, &title, Some(&note));
                req.into_response(200, None, HTML)?
                    .write_all(page.as_bytes())?;
            }
            Ok::<(), anyhow::Error>(())
        })?;
    }

    saved_rx.recv()?;
    // Let the browser receive the reply before the restart cuts it off.
    std::thread::sleep(std::time::Duration::from_secs(2));
    drop(server);
    Ok(())
}

fn page(store: &Store, fields: &[Field], title: &str, note: Option<&str>) -> String {
    let with_values: Vec<_> = fields.iter().map(|f| (*f, store.get(f.key))).collect();
    setup_page::render(title, &with_values, note)
}
