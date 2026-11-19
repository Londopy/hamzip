//! Byte-level BPE encoder matching SmolLM2's tokenizer: digits split one at a
//! time, then the GPT-2 pre-tokenizer pattern, then merges by rank. Decoding
//! never needs this; a different split still decodes, it just compresses worse.

use std::collections::HashMap;

use crate::model::Model;

pub struct Tokenizer {
    byte_token: [u32; 256],
    merges: HashMap<(u32, u32), (u32, u32)>,
}

impl Tokenizer {
    pub fn new(m: &Model) -> Tokenizer {
        let mut byte_token = [u32::MAX; 256];
        for id in 0..m.cfg.vocab as u32 {
            if let [b] = m.token_bytes(id) {
                if byte_token[*b as usize] == u32::MAX {
                    byte_token[*b as usize] = id;
                }
            }
        }
        let merges = m
            .merges
            .iter()
            .enumerate()
            .map(|(rank, &[a, b, out])| ((a, b), (rank as u32, out)))
            .collect();
        Tokenizer { byte_token, merges }
    }

    /// None when the text holds a byte the vocabulary has no token for (a few
    /// control characters and rare UTF-8 lead bytes in SmolLM2).
    pub fn encode(&self, text: &str) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        for piece in pretokenize(text) {
            self.bpe(piece.as_bytes(), &mut out)?;
        }
        Some(out)
    }

    fn bpe(&self, bytes: &[u8], out: &mut Vec<u32>) -> Option<()> {
        let mut word = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match self.byte_token[b as usize] {
                u32::MAX => return None,
                t => word.push(t),
            }
        }
        loop {
            let best = word
                .windows(2)
                .enumerate()
                .filter_map(|(i, p)| {
                    self.merges
                        .get(&(p[0], p[1]))
                        .map(|&(rank, id)| (rank, i, id))
                })
                .min();
            let Some((rank, _, id)) = best else { break };
            // merge every occurrence of this pair, left to right
            let mut i = 0;
            let mut merged = Vec::with_capacity(word.len());
            while i < word.len() {
                if i + 1 < word.len()
                    && self.merges.get(&(word[i], word[i + 1])).map(|m| m.0) == Some(rank)
                {
                    merged.push(id);
                    i += 2;
                } else {
                    merged.push(word[i]);
                    i += 1;
                }
            }
            word = merged;
        }
        out.extend(word);
        Some(())
    }
}

enum Class {
    Letter,
    Number,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphabetic() {
        Class::Letter
    } else if c.is_numeric() {
        Class::Number
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

fn same(a: &Class, b: &Class) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn pretokenize(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_digit() {
            if start < i {
                split_gpt2(&text[start..i], true, &mut out);
            }
            out.push(&text[i..i + 1]);
            start = i + 1;
        }
    }
    if start < text.len() {
        split_gpt2(&text[start..], false, &mut out);
    }
    out
}

const CONTRACTIONS: [&str; 7] = ["'s", "'t", "'re", "'ve", "'m", "'ll", "'d"];

/// `'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+`
///
/// Hugging Face's lookahead sees past the segment into the digit that cut it,
/// so `digit_next` says whether the segment is followed by one.
fn split_gpt2<'a>(s: &'a str, digit_next: bool, out: &mut Vec<&'a str>) {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let end_of = |k: usize| chars.get(k).map_or(s.len(), |&(i, _)| i);
    let mut k = 0;
    while k < chars.len() {
        let (i, c) = chars[k];
        if let Some(con) = CONTRACTIONS.iter().find(|con| s[i..].starts_with(*con)) {
            out.push(&s[i..i + con.len()]);
            k += con.chars().count();
            continue;
        }
        let mut j = k;
        if c == ' ' && k + 1 < chars.len() && !matches!(class(chars[k + 1].1), Class::Space) {
            j += 1;
        }
        let first = class(chars[j].1);
        if !matches!(first, Class::Space) {
            let mut e = j + 1;
            while e < chars.len() && same(&class(chars[e].1), &first) {
                e += 1;
            }
            out.push(&s[i..end_of(e)]);
            k = e;
            continue;
        }
        let mut e = k + 1;
        while e < chars.len() && matches!(class(chars[e].1), Class::Space) {
            e += 1;
        }
        // leave the last space of a run for the word after it
        if (e < chars.len() || digit_next) && e - k > 1 {
            e -= 1;
        }
        out.push(&s[i..end_of(e)]);
        k = e;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretokenizer_splits_like_gpt2() {
        assert_eq!(
            pretokenize("it's 146.5 MHz"),
            ["it", "'s", " ", "1", "4", "6", ".", "5", " MHz"]
        );
        assert_eq!(
            pretokenize("  two  spaces\t tab"),
            [" ", " two", " ", " spaces", "\t", " tab"]
        );
        assert_eq!(
            pretokenize("73\n\nTO: EOC"),
            ["7", "3", "\n", "\n", "TO", ":", " EOC"]
        );
        assert_eq!(pretokenize("end  "), ["end", "  "]);
        assert_eq!(
            pretokenize("slow.\n\n73"),
            ["slow", ".", "\n", "\n", "7", "3"]
        );
        assert_eq!(pretokenize("XQT  73"), ["XQT", " ", " ", "7", "3"]);
        assert_eq!(pretokenize("a ...b"), ["a", " ...", "b"]);
    }
}
