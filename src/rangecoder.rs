//! Range coder with a 64-bit range, so frequency totals up to 2^24 still keep
//! at least 32 bits of precision per symbol. Carry handling follows LZMA.

const TOP: u64 = 1 << 56;

pub struct Encoder {
    low: u128,
    range: u64,
    cache: u8,
    pending: u64,
    started: bool,
    out: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder {
            low: 0,
            range: u64::MAX,
            cache: 0,
            pending: 0,
            started: false,
            out: Vec::new(),
        }
    }

    pub fn encode(&mut self, start: u32, size: u32, total: u32) {
        let r = self.range / total as u64;
        self.low += r as u128 * start as u128;
        self.range = r * size as u64;
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u64) < 0xFF << 56 || self.low >> 64 != 0 {
            let carry = (self.low >> 64) as u8;
            // the first byte is always zero (the interval starts below 2^64), so it is never sent
            if self.started {
                self.out.push(self.cache.wrapping_add(carry));
            }
            self.started = true;
            for _ in 0..self.pending {
                self.out.push(0xFFu8.wrapping_add(carry));
            }
            self.pending = 0;
            self.cache = (self.low >> 56) as u8;
        } else {
            self.pending += 1;
        }
        self.low = (self.low & 0x00FF_FFFF_FFFF_FFFF) << 8;
    }

    /// Ends the stream with as few bytes as possible. The decoder reads missing
    /// bytes as zero, so trailing zero bytes are dropped too.
    pub fn finish(mut self) -> Vec<u8> {
        // any value in [low, low + range) decodes the same; pick the one with the
        // most trailing zeros, which needs at most one more byte
        self.low = (self.low + (TOP - 1) as u128) & !((TOP - 1) as u128);
        self.shift_low();
        self.shift_low();
        while self.out.last() == Some(&0) {
            self.out.pop();
        }
        self.out
    }
}

pub struct Decoder<'a> {
    code: u64,
    range: u64,
    input: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(input: &'a [u8]) -> Decoder<'a> {
        let mut d = Decoder {
            code: 0,
            range: u64::MAX,
            input,
            pos: 0,
        };
        for _ in 0..8 {
            d.code = (d.code << 8) | d.next() as u64;
        }
        d
    }

    fn next(&mut self) -> u8 {
        let b = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    /// A valid stream reads at most 8 bytes past its end plus the zero bytes the
    /// encoder trimmed; 32 trimmed zeros in a row would take a 2^-256 accident.
    pub fn overrun(&self) -> bool {
        self.pos > self.input.len() + 40
    }

    /// The value to look up in the cumulative table; a symbol `s` matches when
    /// `start[s] <= value < start[s] + size[s]`.
    pub fn target(&self, total: u32) -> u32 {
        let v = self.code / (self.range / total as u64);
        v.min(total as u64 - 1) as u32
    }

    pub fn consume(&mut self, start: u32, size: u32, total: u32) {
        let r = self.range / total as u64;
        self.code -= r * start as u64;
        self.range = r * size as u64;
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // xorshift, so the test needs no rand dependency
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn roundtrip(freqs: &[u32], symbols: &[usize]) -> usize {
        let mut cum = vec![0u32];
        for f in freqs {
            cum.push(cum.last().unwrap() + f);
        }
        let total = *cum.last().unwrap();
        let mut e = Encoder::new();
        for &s in symbols {
            e.encode(cum[s], freqs[s], total);
        }
        let bytes = e.finish();
        let mut d = Decoder::new(&bytes);
        for &s in symbols {
            let t = d.target(total);
            let got = cum.partition_point(|&c| c <= t) - 1;
            assert_eq!(got, s);
            d.consume(cum[got], freqs[got], total);
        }
        bytes.len()
    }

    #[test]
    fn roundtrips_random_streams() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for trial in 0..200 {
            let n = 2 + (rng.next() % 300) as usize;
            let skew = trial % 3;
            let freqs: Vec<u32> = (0..n)
                .map(|_| 1 + (rng.next() % [3, 1000, 60000][skew]) as u32)
                .collect();
            let len = (rng.next() % 500) as usize;
            let symbols: Vec<usize> = (0..len).map(|_| (rng.next() % n as u64) as usize).collect();
            roundtrip(&freqs, &symbols);
        }
    }

    #[test]
    fn roundtrips_near_certain_and_rare_symbols() {
        // 2^24 total with one symbol at nearly all the mass forces long runs of 0xFF and carries
        let mut freqs = vec![1u32; 49152];
        freqs[7] = (1 << 24) - 49151;
        let symbols: Vec<usize> = (0..5000)
            .map(|i| if i % 97 == 0 { i % 49152 } else { 7 })
            .collect();
        roundtrip(&freqs, &symbols);
    }

    #[test]
    fn size_is_close_to_entropy() {
        // 1000 symbols at p = 1/2 is 125 bytes; finishing costs at most one more, and
        // trailing zero bytes are trimmed, so a few fewer is possible
        let len = roundtrip(
            &[1, 1],
            &(0..1000).map(|i| (i * 7 % 3) % 2).collect::<Vec<_>>(),
        );
        assert!((120..=126).contains(&len), "{len} bytes");
    }
}
