//! Text form of a message for modes that only carry text (JS8Call, fldigi, a
//! Winlink message body): `hz:` followed by standard base64 (RFC 4648).

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

/// The binary message inside `input` if it is in text form, ignoring
/// surrounding whitespace and line breaks a text mode may have added.
pub fn from_text(input: &[u8]) -> Option<Result<Vec<u8>, String>> {
    let s = std::str::from_utf8(input).ok()?.trim();
    let body = s.strip_prefix(PREFIX)?;
    let chars: Vec<u8> = body.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
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
        assert!(from_text(b"plain text").is_none());
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
