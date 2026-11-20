//! Shared-model text compression for slow radio links. See docs/wire-format.md.

pub mod armor;
mod engine;
mod model;
pub mod net;
mod rangecoder;
pub mod store;
mod tokenizer;

use std::fmt;
use std::io::Read;

use rayon::prelude::*;

pub use model::Model;
pub use tokenizer::Tokenizer;

const FORMAT: u8 = 1;
/// The hamzip release this is; the CLI and the GUI share it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Text both stations put in front of every model-coded message. It costs
/// nothing on air and gives the model a start, saving about 1% on average.
pub const CONTEXT: &str = "Amateur radio message sent over Winlink:\n\n";
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
const MAX_MESSAGE: u64 = 1 << 20;

/// The published models, by name and the SHA-256 of the model file. The first is
/// the default. A message's two model id bytes are the start of that hash.
pub const MODELS: &[(&str, &str)] = &[
    (
        "smollm2-135m",
        "4f3b006a37c45d5f7ac496eba7bae7deef47661feeed1891b3340e18b4b9c919",
    ),
    (
        "smollm2-360m",
        "6f5678c020f9979fe9628fa379c4ad1d37e36f2cdfb5f1ed714b13e4ecfd6983",
    ),
];

fn id_of(sha256_hex: &str) -> [u8; 2] {
    let byte = |i: usize| u8::from_str_radix(&sha256_hex[i..i + 2], 16).unwrap();
    [byte(0), byte(2)]
}

/// The SHA-256 of the published model with this name.
pub fn model_sha256(name: &str) -> Option<&'static str> {
    MODELS.iter().find(|(n, _)| *n == name).map(|(_, sha)| *sha)
}

/// The name of the published model with this message id, if there is one.
pub fn model_name(id: [u8; 2]) -> Option<&'static str> {
    MODELS
        .iter()
        .find(|(_, sha)| id_of(sha) == id)
        .map(|(name, _)| *name)
}

/// A model id as people should read it: its name when it is a published model.
pub fn describe_model(id: [u8; 2]) -> String {
    let hex = format!("{:02x}{:02x}", id[0], id[1]);
    match model_name(id) {
        Some(name) => format!("{name} ({hex}..)"),
        None => format!("{hex}.., not a published model"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Model = 0,
    Zstd = 1,
    Raw = 2,
}

#[derive(Debug)]
pub enum Error {
    Io(String),
    BadModel(String),
    BadMessage(String),
    NeedModel,
    WrongModel { sender: [u8; 2], ours: [u8; 2] },
    Checksum,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::BadModel(e) => write!(f, "bad model file: {e}"),
            Error::BadMessage(e) => write!(f, "bad message: {e}"),
            Error::NeedModel => write!(f, "this message was coded with a model; pass --model"),
            Error::WrongModel { sender, ours } => {
                write!(
                    f,
                    "this message was coded with model {}, but the loaded model is {}",
                    describe_model(*sender),
                    describe_model(*ours)
                )?;
                if let Some(name) = model_name(*sender) {
                    write!(
                        f,
                        "; pass --model {name} if you have it, or get it with \
                         `hamzip model download {name}`"
                    )?;
                }
                Ok(())
            }
            Error::Checksum => write!(f, "checksum mismatch: the message is corrupt"),
        }
    }
}

impl std::error::Error for Error {}

/// Header byte, then mode-specific payload, then CRC-32 of the original text.
pub fn mode_of(msg: &[u8]) -> Result<Mode, Error> {
    let h = *msg
        .first()
        .ok_or_else(|| Error::BadMessage("empty".into()))?;
    if h >> 4 != FORMAT {
        return Err(Error::BadMessage(format!(
            "format {}, this hamzip reads format {FORMAT}",
            h >> 4
        )));
    }
    match h & 0x0F {
        0 => Ok(Mode::Model),
        1 => Ok(Mode::Zstd),
        2 => Ok(Mode::Raw),
        m => Err(Error::BadMessage(format!("unknown mode {m}"))),
    }
}

fn frame(mode: Mode, body: &[u8], text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(FORMAT << 4 | mode as u8);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc32fast::hash(text).to_le_bytes());
    out
}

/// Codes token ids with the model; the sequence ends with the end-of-text token.
pub fn encode_tokens(m: &Model, tokens: &[u32]) -> Vec<u8> {
    encode_in_blocks(m, tokens, 64)
}

/// Starts a session that has seen the start token and the shared context, and
/// returns it with the last of those tokens, which predicts the message's first.
fn primed(m: &Model) -> (engine::Session<'_>, u32) {
    let mut s = engine::Session::new(m);
    let prefix: Vec<u32> = std::iter::once(m.cfg.eos)
        .chain(m.context.iter().copied())
        .collect();
    let (&last, rest) = prefix.split_last().unwrap();
    if !rest.is_empty() {
        s.steps(rest);
    }
    (s, last)
}

// the encoder knows every token up front, so it runs the model on blocks of them
fn encode_in_blocks(m: &Model, tokens: &[u32], block: usize) -> Vec<u8> {
    let (mut s, first) = primed(m);
    let inputs: Vec<u32> = std::iter::once(first)
        .chain(tokens.iter().copied())
        .collect();
    let targets: Vec<u32> = tokens
        .iter()
        .copied()
        .chain(std::iter::once(m.cfg.eos))
        .collect();
    let mut enc = rangecoder::Encoder::new();
    for (ins, outs) in inputs.chunks(block).zip(targets.chunks(block)) {
        let cums: Vec<Vec<u32>> = s
            .steps(ins)
            .par_iter()
            .map(|l| engine::cumulative(m, l))
            .collect();
        for (cum, &t) in cums.iter().zip(outs) {
            let (lo, hi) = (cum[t as usize], cum[t as usize + 1]);
            enc.encode(lo, hi - lo, *cum.last().unwrap());
        }
    }
    enc.finish()
}

pub fn decode_tokens(m: &Model, data: &[u8]) -> Result<Vec<u32>, Error> {
    let (mut s, mut prev) = primed(m);
    let mut dec = rangecoder::Decoder::new(data);
    let mut out = Vec::new();
    loop {
        if s.pos() == m.cfg.max_ctx || dec.overrun() {
            return Err(Error::BadMessage(
                "ran out of data before the end of message".into(),
            ));
        }
        let cum = engine::cumulative(m, &s.step(prev));
        let total = *cum.last().unwrap();
        let target = dec.target(total);
        let t = cum.partition_point(|&c| c <= target) - 1;
        dec.consume(cum[t], cum[t + 1] - cum[t], total);
        if t as u32 == m.cfg.eos {
            return Ok(out);
        }
        out.push(t as u32);
        prev = t as u32;
    }
}

/// The longest token sequence the model can code: the start token and the shared
/// context take the first positions.
pub fn max_tokens(m: &Model) -> usize {
    m.cfg.max_ctx - 1 - m.context.len()
}

fn zstd_body(text: &[u8]) -> Vec<u8> {
    let mut c = zstd::bulk::Compressor::new(19).expect("zstd level 19 is valid");
    for p in [
        zstd::zstd_safe::CParameter::ContentSizeFlag(false),
        zstd::zstd_safe::CParameter::ChecksumFlag(false),
        zstd::zstd_safe::CParameter::DictIdFlag(false),
    ] {
        c.set_parameter(p).expect("stable zstd parameter");
    }
    let frame = c
        .compress(text)
        .expect("zstd compression of an in-memory buffer");
    // the magic number is constant, so it isn't sent
    frame[4..].to_vec()
}

/// Compresses with whichever of model, zstd and raw comes out smallest.
/// Without a model, only zstd and raw are tried.
pub fn compress(m: Option<&Model>, tok: Option<&Tokenizer>, text: &[u8]) -> Vec<u8> {
    let mut best = frame(Mode::Raw, text, text);
    // the decoder refuses zstd output over MAX_MESSAGE, so bigger text goes raw
    if text.len() as u64 <= MAX_MESSAGE {
        let z = frame(Mode::Zstd, &zstd_body(text), text);
        if z.len() < best.len() {
            best = z;
        }
    }
    // text this long can't fit the context, and BPE on it would be slow
    let fits = |m: &Model| text.len() <= max_tokens(m) * 16;
    if let (Some(m), Some(tok), Ok(s)) = (m.filter(|m| fits(m)), tok, std::str::from_utf8(text)) {
        let tokens = tok.encode(s).unwrap_or_default();
        if !tokens.is_empty() && tokens.len() <= max_tokens(m) {
            let mut body = m.id().to_vec();
            body.extend(encode_tokens(m, &tokens));
            let coded = frame(Mode::Model, &body, text);
            if coded.len() < best.len() {
                best = coded;
            }
        }
    }
    best
}

pub fn decompress(m: Option<&Model>, msg: &[u8]) -> Result<Vec<u8>, Error> {
    let mode = mode_of(msg)?;
    if msg.len() < 5 {
        return Err(Error::BadMessage("too short".into()));
    }
    let (body, crc) = msg[1..].split_at(msg.len() - 5);
    let text = match mode {
        Mode::Raw => body.to_vec(),
        Mode::Zstd => {
            let mut frame = ZSTD_MAGIC.to_vec();
            frame.extend_from_slice(body);
            let mut out = Vec::new();
            zstd::stream::Decoder::new(&frame[..])
                .and_then(|d| d.take(MAX_MESSAGE + 1).read_to_end(&mut out))
                .map_err(|e| Error::BadMessage(format!("zstd: {e}")))?;
            if out.len() as u64 > MAX_MESSAGE {
                return Err(Error::BadMessage("zstd text is longer than 1 MiB".into()));
            }
            out
        }
        Mode::Model => {
            let m = m.ok_or(Error::NeedModel)?;
            if body.len() < 2 {
                return Err(Error::BadMessage("too short".into()));
            }
            let sender = [body[0], body[1]];
            if sender != m.id() {
                return Err(Error::WrongModel {
                    sender,
                    ours: m.id(),
                });
            }
            let mut out = Vec::new();
            for t in decode_tokens(m, &body[2..])? {
                out.extend_from_slice(m.token_bytes(t));
            }
            out
        }
    };
    if crc32fast::hash(&text).to_le_bytes() != crc {
        return Err(Error::Checksum);
    }
    Ok(text)
}

/// The shipped model, loaded once for all tests. Tests that need it fail rather than
/// skip when it is missing.
#[cfg(test)]
fn test_model() -> &'static Model {
    static MODEL: std::sync::OnceLock<Model> = std::sync::OnceLock::new();
    MODEL.get_or_init(|| {
        let path = std::env::var_os("HAMZIP_MODEL").map(std::path::PathBuf::from).unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models/smollm2-135m.hzm")
        });
        Model::load(&path).unwrap_or_else(|e| {
            panic!("these tests need the model file ({e}); build it with tools/convert.py or set HAMZIP_MODEL")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET: &[u8] = b"NET REPORT 11/18 NCS KE8TWV. Check-ins: AG6XN, N0QFR, KD9XQT, \
        W4ZKB, N7RPL. Traffic: 2 formal, 1 welfare. Next net 11/25 1900L on 146.940 PL 100.0. \
        Thanks to all who checked in tonight.";

    #[test]
    fn short_text_goes_raw_and_round_trips() {
        let msg = compress(None, None, b"QSL 73");
        assert_eq!(mode_of(&msg).unwrap(), Mode::Raw);
        assert_eq!(msg.len(), 6 + 5);
        assert_eq!(decompress(None, &msg).unwrap(), b"QSL 73");
    }

    #[test]
    fn longer_text_goes_zstd_without_a_model() {
        let msg = compress(None, None, NET);
        assert_eq!(mode_of(&msg).unwrap(), Mode::Zstd);
        assert!(msg.len() < NET.len());
        assert_eq!(decompress(None, &msg).unwrap(), NET);
    }

    #[test]
    fn corruption_is_caught() {
        for flip in [1, 10, NET.len() / 3] {
            let mut msg = compress(None, None, NET);
            msg[flip] ^= 0x20;
            assert!(
                decompress(None, &msg).is_err(),
                "flip at {flip} went unnoticed"
            );
        }
    }

    #[test]
    fn model_message_needs_a_model() {
        let msg = frame(Mode::Model, &[0xAB, 0xCD, 1, 2, 3], b"x");
        assert!(matches!(decompress(None, &msg), Err(Error::NeedModel)));
    }

    #[test]
    fn newer_format_is_refused_by_name() {
        let mut msg = compress(None, None, b"QSL 73");
        msg[0] = 2 << 4;
        let err = decompress(None, &msg).unwrap_err().to_string();
        assert!(err.contains("format 2"), "{err}");
    }

    #[test]
    fn text_over_the_zstd_limit_goes_raw() {
        let text = vec![b'a'; MAX_MESSAGE as usize + 1];
        let msg = compress(None, None, &text);
        assert_eq!(mode_of(&msg).unwrap(), Mode::Raw);
        assert_eq!(decompress(None, &msg).unwrap(), text);
    }

    #[test]
    fn shared_context_has_the_published_token_ids() {
        // docs/wire-format.md lists these; Hugging Face's tokenizer gives the same
        let want = [8112, 17919, 5171, 3714, 2362, 690, 16641, 8371, 42, 1116];
        assert_eq!(test_model().context, want);
    }

    #[test]
    fn a_message_exactly_at_the_token_limit_round_trips() {
        let m = test_model();
        let tok = Tokenizer::new(m);
        let base = tok.encode(std::str::from_utf8(NET).unwrap()).unwrap();
        let tokens: Vec<u32> = base.iter().copied().cycle().take(max_tokens(m)).collect();
        let coded = encode_tokens(m, &tokens);
        assert_eq!(decode_tokens(m, &coded).unwrap(), tokens);
    }

    #[test]
    fn block_size_does_not_change_the_bytes() {
        let m = test_model();
        let tokens = Tokenizer::new(m)
            .encode(std::str::from_utf8(NET).unwrap())
            .unwrap();
        let one = encode_in_blocks(m, &tokens, 1);
        assert_eq!(encode_in_blocks(m, &tokens, 7), one);
        assert_eq!(encode_in_blocks(m, &tokens, 64), one);
    }

    #[test]
    fn model_beats_zstd_and_round_trips() {
        let m = test_model();
        let tok = Tokenizer::new(m);
        let msg = compress(Some(m), Some(&tok), NET);
        assert_eq!(mode_of(&msg).unwrap(), Mode::Model);
        assert!(
            msg.len() * 2 < compress(None, None, NET).len(),
            "{} bytes",
            msg.len()
        );
        assert_eq!(decompress(Some(m), &msg).unwrap(), NET);
    }

    #[test]
    fn odd_text_round_trips() {
        let m = test_model();
        let tok = Tokenizer::new(m);
        for text in [
            "x",
            "line one\r\nline two\r\n",
            "literal <|endoftext|> in the middle",
            "73 de K9XYZ \u{1F4E1} ok",
            "tabs\tand  double  spaces \n\n\n end",
        ] {
            let msg = compress(Some(m), Some(&tok), text.as_bytes());
            assert_eq!(
                decompress(Some(m), &msg).unwrap(),
                text.as_bytes(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn corrupt_model_message_fails_fast() {
        let m = test_model();
        let tok = Tokenizer::new(m);
        let good = compress(Some(m), Some(&tok), NET);
        for i in [3, good.len() / 2, good.len() - 5] {
            let mut msg = good.clone();
            msg[i] ^= 0x01;
            assert!(
                decompress(Some(m), &msg).is_err(),
                "flip at {i} went unnoticed"
            );
        }
    }

    #[test]
    fn wrong_model_is_named() {
        let m = test_model();
        let mut msg = frame(Mode::Model, &[0, 0, 1, 2, 3], b"x");
        msg[1] = m.id()[0] ^ 0xFF;
        let err = decompress(Some(m), &msg).unwrap_err();
        assert!(matches!(err, Error::WrongModel { .. }), "{err}");
    }

    #[test]
    fn a_message_for_the_other_published_model_names_it() {
        let m = test_model();
        let other = id_of(MODELS[1].1);
        let msg = frame(Mode::Model, &[other[0], other[1], 1, 2, 3], b"x");
        let err = decompress(Some(m), &msg).unwrap_err().to_string();
        assert!(
            err.contains("smollm2-360m (6f56..)") && err.contains("smollm2-135m (4f3b..)"),
            "{err}"
        );
        assert!(err.contains("hamzip model download smollm2-360m"), "{err}");
    }

    #[test]
    fn published_models_have_distinct_ids_and_the_default_is_shipped() {
        let ids: std::collections::HashSet<_> = MODELS.iter().map(|(_, s)| id_of(s)).collect();
        assert_eq!(ids.len(), MODELS.len());
        let sha: String = test_model()
            .sha256
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(sha, MODELS[0].1);
    }
}
