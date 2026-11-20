//! What the window shows about a message, kept apart from drawing it so it can
//! be tested without a display.

use hamzip::Mode;

/// Link speeds offered for the airtime estimate, in bits per second.
pub const RATES: [u32; 5] = [300, 1200, 2400, 4800, 9600];

/// Sizes of one compressed message next to the text it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub mode: Mode,
    pub chars: usize,
    pub text_bytes: usize,
    pub msg_bytes: usize,
    pub text_form_chars: usize,
}

impl Stats {
    pub fn new(text: &str, msg: &[u8]) -> Result<Stats, hamzip::Error> {
        Ok(Stats {
            mode: hamzip::mode_of(msg)?,
            chars: text.chars().count(),
            text_bytes: text.len(),
            msg_bytes: msg.len(),
            text_form_chars: hamzip::armor::to_text(msg).len(),
        })
    }

    /// Bits on air per character of the original, framing included.
    pub fn bits_per_char(&self) -> f64 {
        self.msg_bytes as f64 * 8.0 / self.chars.max(1) as f64
    }

    /// The message's size as a share of the plain text it replaces.
    pub fn percent_of_text(&self) -> f64 {
        100.0 * self.msg_bytes as f64 / self.text_bytes.max(1) as f64
    }
}

pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Model => "model",
        Mode::Zstd => "zstd (the model didn't help here)",
        Mode::Raw => "plain text (too short to compress)",
    }
}

/// Seconds to send `bytes` at `bps`, payload only: the mode's own framing,
/// handshakes and retries come on top.
pub fn airtime(bytes: usize, bps: u32) -> f64 {
    bytes as f64 * 8.0 / bps.max(1) as f64
}

pub fn format_seconds(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.1} s")
    } else if s < 60.0 {
        format!("{s:.0} s")
    } else {
        let s = s.round() as u64;
        format!("{} min {} s", s / 60, s % 60)
    }
}

/// Decimal megabytes, the unit the download sizes are given in.
pub fn format_bytes(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.0} MB", n as f64 / 1e6)
    } else {
        format!("{n} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gui_and_the_library_carry_the_same_version() {
        assert_eq!(env!("CARGO_PKG_VERSION"), hamzip::VERSION);
    }

    #[test]
    fn stats_describe_a_raw_message() {
        let msg = hamzip::compress(None, None, b"QSL 73");
        let s = Stats::new("QSL 73", &msg).unwrap();
        assert_eq!(s.mode, Mode::Raw);
        assert_eq!((s.chars, s.text_bytes, s.msg_bytes), (6, 6, 11));
        assert!((s.bits_per_char() - 88.0 / 6.0).abs() < 1e-9);
        assert_eq!(s.text_form_chars, "hz:".len() + 16);
    }

    #[test]
    fn characters_and_bytes_differ_for_non_ascii_text() {
        let text = "73 de K9XYZ, café";
        let msg = hamzip::compress(None, None, text.as_bytes());
        let s = Stats::new(text, &msg).unwrap();
        assert_eq!(s.chars, 17);
        assert_eq!(s.text_bytes, 18);
        assert!((s.percent_of_text() - 100.0 * msg.len() as f64 / 18.0).abs() < 1e-9);
    }

    #[test]
    fn airtime_is_payload_bits_over_rate() {
        assert_eq!(airtime(150, 300), 4.0);
        assert_eq!(airtime(150, 1200), 1.0);
        assert_eq!(airtime(1, 0), 8.0);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_seconds(0.43), "0.4 s");
        assert_eq!(format_seconds(42.4), "42 s");
        assert_eq!(format_seconds(125.0), "2 min 5 s");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(137_105_551), "137 MB");
    }

    #[test]
    fn every_mode_has_a_label_that_says_why() {
        assert_eq!(mode_label(Mode::Model), "model");
        assert!(mode_label(Mode::Zstd).contains("zstd"));
        assert!(mode_label(Mode::Raw).contains("plain"));
    }
}
