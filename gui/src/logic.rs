//! What the window shows about a message, kept apart from drawing it so it can
//! be tested without a display.

use hamzip::Mode;

/// Tracks a background job whose input keeps changing: only the result for
/// the latest input is shown, and at most one job runs at a time, started once
/// more with the latest input if it changed meanwhile.
#[derive(Default)]
pub struct Latest {
    generation: u64,
    running: bool,
    rerun: bool,
}

impl Latest {
    /// The input changed, so a result still on its way is out of date.
    pub fn changed(&mut self) {
        self.generation += 1;
    }

    /// The generation to start a job for, or None when one is already running;
    /// the input is then picked up when that one finishes.
    pub fn start(&mut self) -> Option<u64> {
        if self.running {
            self.rerun = true;
            return None;
        }
        self.running = true;
        Some(self.generation)
    }

    /// A job finished: whether to show its result, and whether to start again.
    pub fn finish(&mut self, generation: u64) -> (bool, bool) {
        self.running = false;
        (self.is_current(generation), std::mem::take(&mut self.rerun))
    }

    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }
}

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
        Mode::Model => "coded with the model",
        Mode::Zstd => "zstd, smaller here than the model, or the text is too long for it",
        Mode::Raw => "as plain text, which came out smallest",
    }
}

/// One line on how a received message was sent, for above its decoded text.
pub fn received_summary(mode: Mode, bytes: usize, model: Option<&str>) -> String {
    match (mode, model) {
        (Mode::Model, Some(m)) => format!("Coded with {m}, {bytes} bytes on air"),
        (Mode::Zstd, _) => format!("Sent as zstd, {bytes} bytes on air"),
        _ => format!("Sent as plain text, {bytes} bytes on air"),
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
        // per character, not per byte
        assert!((s.bits_per_char() - msg.len() as f64 * 8.0 / 17.0).abs() < 1e-9);
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
        assert_eq!(format_seconds(9.94), "9.9 s");
        assert_eq!(format_seconds(10.0), "10 s");
        assert_eq!(format_seconds(42.4), "42 s");
        assert_eq!(format_seconds(59.4), "59 s");
        assert_eq!(format_seconds(60.0), "1 min 0 s");
        assert_eq!(format_seconds(125.0), "2 min 5 s");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(999_999), "999999 bytes");
        assert_eq!(format_bytes(1_000_000), "1 MB");
        assert_eq!(format_bytes(137_105_551), "137 MB");
    }

    #[test]
    fn every_mode_has_its_own_label() {
        assert!(mode_label(Mode::Model).contains("model"));
        assert!(mode_label(Mode::Zstd).starts_with("zstd"));
        assert!(mode_label(Mode::Raw).contains("plain text"));
    }

    #[test]
    fn a_result_for_text_since_edited_is_not_shown() {
        let mut jobs = Latest::default();
        let first = jobs.start().unwrap();
        jobs.changed();
        // still running, so the new text waits for it
        assert_eq!(jobs.start(), None);
        assert_eq!(jobs.finish(first), (false, true));
        let second = jobs.start().unwrap();
        assert_eq!(jobs.finish(second), (true, false));
        // nothing is running now, so the next edit starts at once
        jobs.changed();
        assert!(jobs.start().is_some());
    }

    #[test]
    fn an_edit_after_the_last_job_makes_its_result_stale() {
        let mut jobs = Latest::default();
        let done = jobs.start().unwrap();
        assert_eq!(jobs.finish(done), (true, false));
        jobs.changed();
        assert!(!jobs.is_current(done));
    }

    #[test]
    fn a_received_message_names_its_model() {
        assert_eq!(
            received_summary(Mode::Model, 62, Some("smollm2-135m (4f3b..)")),
            "Coded with smollm2-135m (4f3b..), 62 bytes on air"
        );
        assert_eq!(
            received_summary(Mode::Raw, 11, None),
            "Sent as plain text, 11 bytes on air"
        );
        assert_eq!(
            received_summary(Mode::Zstd, 40, None),
            "Sent as zstd, 40 bytes on air"
        );
    }
}
