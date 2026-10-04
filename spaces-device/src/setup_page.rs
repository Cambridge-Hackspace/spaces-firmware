//! The setup page a module serves from its own access point.
//!
//! Rendering is pure, so what reaches a browser — and so what comes back from
//! it — is tested here rather than on a board.

use crate::form::escape;

/// One setting the page asks for.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// Where it is stored. On ESP-IDF this is an NVS key: at most 15 characters.
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// Never written back into the page once saved.
    pub secret: bool,
    /// Must be filled in before the module can run.
    pub required: bool,
}

/// Head shared by every page. `charset` matters more than it looks: without it
/// a browser may submit anything outside basic Latin in a legacy encoding, or
/// as an HTML numeric reference, and Spaces device invites are emoji.
const HEAD: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width\">\
<style>body{font-family:sans-serif;max-width:32em;margin:2em auto;padding:0 1em}\
label{display:block;margin-top:1em}input{width:100%;padding:.4em;box-sizing:border-box}\
small{color:#666}.note{background:#fee;padding:.6em}button{margin-top:1.5em;padding:.6em 2em}\
</style>";

/// The form. `fields` pairs each field with its saved value, if any. Secret
/// values are never written into the page; a blank secret on submit means
/// "keep the saved one".
pub fn render(title: &str, fields: &[(Field, Option<String>)], note: Option<&str>) -> String {
    let mut rows = String::new();
    for (field, saved) in fields {
        let saved = saved.as_deref().unwrap_or("");
        let (kind, value, placeholder) = if field.secret {
            let placeholder = if saved.is_empty() {
                ""
            } else {
                "saved; leave blank to keep"
            };
            ("password", String::new(), placeholder)
        } else {
            ("text", escape(saved), "")
        };
        rows.push_str(&format!(
            "<label>{label}{star}<br><input name=\"{key}\" type=\"{kind}\" value=\"{value}\" \
             placeholder=\"{placeholder}\" autocomplete=\"off\"></label><small>{hint}</small>",
            label = escape(field.label),
            star = if field.required { " *" } else { "" },
            key = escape(field.key),
            hint = escape(field.hint),
        ));
    }
    format!(
        "{HEAD}<title>{title}</title></head><body><h2>{title}</h2>{note}\
         <form method=\"post\" action=\"/save\" accept-charset=\"utf-8\">{rows}\
         <button>Save</button></form></body></html>",
        title = escape(title),
        note = note
            .map(|n| format!("<p class=\"note\">{}</p>", escape(n)))
            .unwrap_or_default(),
    )
}

/// What the browser sees after a successful save, just before the restart.
pub fn render_saved(title: &str) -> String {
    format!(
        "{HEAD}<title>{title}</title></head><body><h2>{title}</h2>\
         <p>Saved. Restarting to join the network.</p></body></html>",
        title = escape(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::parse_form;

    const SSID: Field = Field {
        key: "wifi_ssid",
        label: "Wi-Fi network",
        hint: "",
        secret: false,
        required: true,
    };
    const INVITE: Field = Field {
        key: "invite",
        label: "Device invite",
        hint: "from /admin/devices",
        secret: true,
        required: false,
    };

    #[test]
    fn the_page_declares_utf8_where_the_browser_looks() {
        // Without these, an emoji invite can arrive as a legacy encoding or as
        // &#128512; and is refused as an unknown code.
        let page = render("Setup", &[(SSID, None)], None);
        assert!(page.contains("<meta charset=\"utf-8\">"));
        assert!(page.contains("accept-charset=\"utf-8\""));
        assert!(render_saved("Setup").contains("<meta charset=\"utf-8\">"));
    }

    #[test]
    fn an_emoji_invite_survives_the_round_trip() {
        // What a browser on a UTF-8 page sends for a typical invite, including
        // the invisible variation selector that makes an emoji colourful.
        let invite = "🦀🔧❤\u{fe0f}";
        let encoded: String = invite.bytes().map(|b| format!("%{b:02X}")).collect();
        let form = parse_form(&format!("invite={encoded}"));
        assert_eq!(form["invite"], invite, "byte for byte, nothing normalised");
    }

    #[test]
    fn a_saved_secret_is_never_written_into_the_page() {
        let page = render("Setup", &[(INVITE, Some("🦀🔧".into()))], None);
        assert!(!page.contains("🦀"));
        assert!(page.contains("saved; leave blank to keep"));
    }

    #[test]
    fn saved_values_are_escaped() {
        let page = render("Setup", &[(SSID, Some("a\"><script>x".into()))], None);
        assert!(!page.contains("<script>"));
        assert!(page.contains("a&quot;&gt;&lt;script&gt;x"));
    }

    #[test]
    fn a_note_is_escaped_and_required_fields_are_marked() {
        let page = render("Setup", &[(SSID, None)], Some("Still needed: <Wi-Fi>"));
        assert!(page.contains("Still needed: &lt;Wi-Fi&gt;"));
        assert!(page.contains("Wi-Fi network *"));
    }
}
