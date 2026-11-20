//! The hamzip command itself: what a user sees for input that isn't a message.
//! None of these need the model.

use std::io::Write;
use std::process::{Command, Stdio};

fn hamzip(args: &[&str], stdin: &[u8]) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hamzip"))
        .args(args)
        .env("HAMZIP_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

#[test]
fn empty_input_says_so() {
    let (ok, _, err) = hamzip(&["decode"], b"");
    assert!(!ok && err.contains("empty"), "{err}");
}

#[test]
fn a_newer_format_is_named() {
    let (ok, _, err) = hamzip(&["decode"], &[0x20, 1, 2, 3, 4, 5]);
    assert!(!ok && err.contains("message format 2"), "{err}");
}

#[test]
fn plain_text_is_not_mistaken_for_a_message() {
    let (ok, _, err) = hamzip(&["decode"], b"QSL 73 de K9XYZ");
    assert!(!ok && err.contains("no hz: line"), "{err}");
}

#[test]
fn raw_messages_decode_and_describe_without_a_model() {
    // a raw-mode message carries the text as is, then its CRC-32
    let text = b"QSL 73";
    let mut msg = vec![0x12];
    msg.extend_from_slice(text);
    msg.extend_from_slice(&crc32(text).to_le_bytes());
    let (ok, out, err) = hamzip(&["decode", "--model", "missing.hzm"], &msg);
    assert!(ok, "{err}");
    assert_eq!(out.as_bytes(), text);
    let (ok, out, _) = hamzip(&["info"], &msg);
    assert!(ok && out.contains("raw mode, 11 bytes"), "{out}");
}

#[test]
fn a_truncated_message_is_refused_by_info() {
    let (ok, _, err) = hamzip(&["info"], &[0x10, 0xAB]);
    assert!(!ok && err.contains("too short"), "{err}");
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}
