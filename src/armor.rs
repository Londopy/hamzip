//! Text form of a message for modes that only carry text (fldigi keyboard
//! modes, a Winlink message body): `hz:` followed by standard base64 (RFC 4648).
//! The mode has to pass upper and lower case, digits, `+`, `/`, `=` and `:`.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub const PREFIX: &str = "hz:";

pub fn to_text(msg: &[u8]) -> String {
    let mut out = String::from(PREFIX);
    for chunk in msg.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The binary message inside `input` if it is in text form: the first line
/// that starts with `hz:` (any case), plus the lines after it that are only
/// base64, which is how a text mode wraps a long one. None means `input` is
/// a binary message, which always starts with its header byte 0x10-0x1F.
pub fn from_text(input: &[u8]) -> Option<Result<Vec<u8>, String>> {
    if input.first().is_some_and(|&b| b >> 4 == 1) {
        return None;
    }
    if input.starts_with(&[0xFF, 0xFE]) || input.starts_with(&[0xFE, 0xFF]) {
        return Some(Err("this is UTF-16 text; save it as UTF-8".into()));
    }
    let Ok(s) = std::str::from_utf8(input) else {
        return Some(Err(
            "not a hamzip message (neither binary nor hz: text)".into()
        ));
    };
    let mut lines = s.trim_start_matches('\u{feff}').lines().map(str::trim);
    let Some(first) = lines.find(|l| l.get(..3).is_some_and(|p| p.eq_ignore_ascii_case(PREFIX)))
    else {
        return Some(Err("no line starting with hz: found".into()));
    };
    let mut chars: Vec<u8> = first[3..]
        .bytes()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let is_b64 = |l: &&str| !l.is_empty() && l.bytes().all(|c| ALPHABET.contains(&c) || c == b'=');
    for l in lines.take_while(is_b64) {
        chars.extend(l.bytes());
    }
    Some(decode(&chars))
}

fn decode(chars: &[u8]) -> Result<Vec<u8>, String> {
    if !chars.len().is_multiple_of(4) {
        return Err(format!(
            "{} base64 characters, not a multiple of 4",
            chars.len()
        ));
    }
    let mut out = Vec::with_capacity(chars.len() / 4 * 3);
    let last = (chars.len() / 4).saturating_sub(1);
    for (i, quad) in chars.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i != last) {
            return Err("misplaced '=' padding".into());
        }
        let mut n = 0u32;
        for &c in &quad[..4 - pad] {
            let v = ALPHABET
                .iter()
                .position(|&a| a == c)
                .ok_or_else(|| format!("'{}' is not a base64 character", c as char))?;
            n = n << 6 | v as u32;
        }
        n <<= 6 * pad as u32;
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rfc_4648_examples() {
        for (raw, b64) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(to_text(raw.as_bytes()), format!("hz:{b64}"));
            assert_eq!(
                from_text(format!("hz:{b64}").as_bytes()).unwrap().unwrap(),
                raw.as_bytes()
            );
        }
    }

    #[test]
    fn round_trips_every_byte_and_survives_line_breaks() {
        let msg: Vec<u8> = (0..=255).collect();
        let text = to_text(&msg);
        let wrapped: String = text
            .as_bytes()
            .chunks(20)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n");
        assert_eq!(
            from_text(format!("  {wrapped}\n").as_bytes())
                .unwrap()
                .unwrap(),
            msg
        );
    }

    #[test]
    fn binary_messages_are_not_mistaken_for_text() {
        assert!(from_text(&[0x10, b'h', b'z', b':']).is_none());
        // a raw-mode message carries its text as is, hz: lines included
        assert!(from_text(b"\x12note\nhz:Zm9v\nxxxx").is_none());
    }

    #[test]
    fn finds_the_line_in_what_a_text_mode_hands_over() {
        let foobar = b"foobar".to_vec();
        for input in [
            "\u{feff}hz:Zm9vYmFy\r\n".as_bytes(),
            b"73 de K9XYZ\nHZ:Zm9v\nYmFy\n\nK9XYZ SK",
            b"  hz:Zm9vYmFy  ",
        ] {
            assert_eq!(from_text(input).unwrap().unwrap(), foobar, "{input:?}");
        }
    }

    #[test]
    fn other_text_gets_a_reason_not_a_format_error() {
        assert!(from_text(b"\xff\xfeh\0z\0")
            .unwrap()
            .unwrap_err()
            .contains("UTF-16"));
        assert!(from_text(b"plain text")
            .unwrap()
            .unwrap_err()
            .contains("no line"));
        assert!(from_text(b"\xc3\x28")
            .unwrap()
            .unwrap_err()
            .contains("not a hamzip message"));
    }

    #[test]
    fn bad_text_is_refused_with_a_reason() {
        assert!(from_text(b"hz:Zm9")
            .unwrap()
            .unwrap_err()
            .contains("multiple of 4"));
        assert!(from_text(b"hz:Zm9*").unwrap().unwrap_err().contains("'*'"));
        assert!(from_text(b"hz:Zg==Zm9v")
            .unwrap()
            .unwrap_err()
            .contains("padding"));
    }
}
