//! Small helpers for a device's own web pages: reading a submitted form, and
//! putting untrusted text into HTML. Pure, so they are tested here rather than
//! trusted on a board.

use std::collections::HashMap;

/// Parse an `application/x-www-form-urlencoded` body into a map. A key given
/// twice keeps its last value.
pub fn parse_form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (!k.is_empty()).then(|| (decode(k), decode(v)))
        })
        .collect()
}

/// Undo form encoding: `+` is a space and `%XX` is a byte. A `%` not followed
/// by two hex digits is kept as it is rather than guessed at.
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let byte = bytes
                    .get(i + 1..i + 3)
                    .and_then(|hex| std::str::from_utf8(hex).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match byte {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Make text safe to put inside HTML, including inside a quoted attribute.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_spaces_and_escapes() {
        assert_eq!(decode("a+b%20c"), "a b c");
        assert_eq!(
            decode("mqtt%3A%2F%2F10.0.0.1%3A1883"),
            "mqtt://10.0.0.1:1883"
        );
    }

    #[test]
    fn decodes_an_escape_at_the_very_end() {
        // The off-by-one the traffic light's decoder had: a trailing %XX lost.
        assert_eq!(decode("pass%21"), "pass!");
    }

    #[test]
    fn keeps_a_percent_it_cannot_decode() {
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("50%zz"), "50%zz");
        assert_eq!(decode("%4"), "%4");
    }

    #[test]
    fn decodes_utf8() {
        assert_eq!(decode("caf%C3%A9"), "café");
    }

    #[test]
    fn does_not_panic_on_multibyte_text_after_a_percent() {
        assert_eq!(decode("%é"), "%é");
    }

    #[test]
    fn parses_a_form() {
        let form = parse_form("wifi_ssid=Hack+Space&wifi_pass=&tool_id=laser-01");
        assert_eq!(form["wifi_ssid"], "Hack Space");
        assert_eq!(form["wifi_pass"], "");
        assert_eq!(form["tool_id"], "laser-01");
    }

    #[test]
    fn ignores_empty_pairs_and_keys() {
        let form = parse_form("&=x&a=1&&");
        assert_eq!(form.len(), 1);
        assert_eq!(form["a"], "1");
    }

    #[test]
    fn escapes_everything_that_could_break_out_of_an_attribute() {
        assert_eq!(
            escape(r#"<x a='1' b="2">&"#),
            "&lt;x a=&#39;1&#39; b=&quot;2&quot;&gt;&amp;"
        );
    }
}
