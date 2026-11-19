//! Integer-only Llama forward pass. Every step is exact integer arithmetic with
//! a fixed rounding rule, so any CPU produces the same probabilities bit for bit.
//! Activations are Q12 fixed point (value * 2^12) unless noted.

use rayon::prelude::*;

use crate::model::{Linear, Model};

const ACT_FRAC: i32 = 12;
// rmsnorm epsilon 1e-5 in Q24
const NORM_EPS: i128 = 168;
const TBL_SHIFT: i32 = 6;
const SILU_LIMIT: i64 = 16 << ACT_FRAC;
const EXP_LIMIT: i64 = 24 << ACT_FRAC;
/// Sum of the frequencies handed to the range coder is at most this.
pub const FREQ_TOTAL: u64 = 1 << 24;

// done in i128 and saturated, so even a model at the loader's limits can't wrap
fn round_shift(x: i64, s: i32) -> i64 {
    let x = x as i128;
    let r = if s <= 0 {
        x << (-s).min(64)
    } else {
        round_shift128(x, s)
    };
    r.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn round_shift128(x: i128, s: i32) -> i128 {
    (x + (1 << (s - 1))) >> s
}

fn sat32(x: i64) -> i32 {
    x.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// A vector quantized to i16 for a matrix multiply: value = a * 2^-frac.
struct QVec {
    a: Vec<i16>,
    frac: i32,
}

fn quantize(v: &[i64], frac: i32) -> QVec {
    let max = v.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
    let bits = 64 - max.leading_zeros() as i32;
    let s = (bits - 15).max(0);
    let a = v
        .iter()
        .map(|&x| round_shift(x, s).clamp(-32767, 32767) as i16)
        .collect();
    QVec { a, frac: frac - s }
}

// integer sums are exact in any order, so every dot product variant gives the same result
fn dot(a: &[i16], q: &[i8]) -> i64 {
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") && a.len().is_multiple_of(16) && q.len() == a.len() {
        // SAFETY: avx2 support was just checked, and both slices hold a.len() elements
        return unsafe { dot_avx2(a, q) };
    }
    dot_scalar(a, q)
}

fn dot_scalar(a: &[i16], q: &[i8]) -> i64 {
    // 256 products of |i16| * |i8| stay under 2^31, so each chunk sums exactly in i32
    a.chunks(256)
        .zip(q.chunks(256))
        .map(|(a, q)| {
            a.iter()
                .zip(q)
                .map(|(&x, &w)| x as i32 * w as i32)
                .sum::<i32>() as i64
        })
        .sum()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_avx2(a: &[i16], q: &[i8]) -> i64 {
    use std::arch::x86_64::*;
    let mut total = 0i64;
    for (a, q) in a.chunks(256).zip(q.chunks(256)) {
        let mut acc = _mm256_setzero_si256();
        for i in (0..a.len()).step_by(16) {
            let x = _mm256_loadu_si256(a.as_ptr().add(i) as *const __m256i);
            let w = _mm256_cvtepi8_epi16(_mm_loadu_si128(q.as_ptr().add(i) as *const __m128i));
            acc = _mm256_add_epi32(acc, _mm256_madd_epi16(x, w));
        }
        let mut lanes = [0i32; 8];
        _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, acc);
        total += lanes.iter().map(|&l| l as i64).sum::<i64>();
    }
    total
}

/// Multiplies every vector in `xs` by `w`; each weight row is read once for the whole batch.
fn linear(xs: &[QVec], w: &Linear) -> Vec<Vec<i32>> {
    let n = xs.len();
    let mut out = vec![0i32; w.rows * n];
    out.par_chunks_mut(n)
        .enumerate()
        .with_min_len((64 / n).max(1))
        .for_each(|(r, o)| {
            let row = &w.q[r * w.cols..(r + 1) * w.cols];
            for (o, x) in o.iter_mut().zip(xs) {
                let acc = dot(&x.a, row);
                *o = sat32(round_shift(acc * w.mant[r], w.shift[r] + x.frac - ACT_FRAC));
            }
        });
    (0..n)
        .map(|j| (0..w.rows).map(|r| out[r * n + j]).collect())
        .collect()
}

fn rmsnorm(x: &[i32], gain: &[i32]) -> QVec {
    let ss: i128 = x.iter().map(|&v| v as i128 * v as i128).sum();
    let mean = ss / x.len() as i128 + NORM_EPS;
    // rms in Q20, its inverse in Q42; x Q12 * gain Q16 * inv Q42 = Q70, down to Q28
    let rms = ((mean as u128) << 16).isqrt() as i128;
    let inv = (1i128 << 62) / rms;
    let y: Vec<i64> = x
        .iter()
        .zip(gain)
        .map(|(&v, &g)| round_shift128(v as i128 * g as i128 * inv, 42) as i64)
        .collect();
    quantize(&y, 28)
}

fn silu(m: &Model, x: i32) -> i64 {
    let x = x as i64;
    if x >= SILU_LIMIT {
        return x;
    }
    if x < -SILU_LIMIT {
        return 0;
    }
    let i = x + SILU_LIMIT;
    let (k, fr) = ((i >> TBL_SHIFT) as usize, i & 63);
    let (t0, t1) = (m.silu[k] as i64, m.silu[k + 1] as i64);
    t0 + (((t1 - t0) * fr + 32) >> TBL_SHIFT)
}

/// exp(-d) in Q30 for d >= 0 in Q12.
fn exp_neg(m: &Model, d: i64) -> i64 {
    if d >= EXP_LIMIT {
        return 0;
    }
    let (k, fr) = ((d >> TBL_SHIFT) as usize, d & 63);
    let (t0, t1) = (m.exp[k] as i64, m.exp[k + 1] as i64);
    t0 - (((t0 - t1) * fr + 32) >> TBL_SHIFT)
}

/// One running sequence: feed tokens with `step` and get the next-token logits.
pub struct Session<'m> {
    m: &'m Model,
    keys: Vec<Vec<i32>>,
    values: Vec<Vec<i32>>,
    pos: usize,
}

impl<'m> Session<'m> {
    pub fn new(m: &'m Model) -> Session<'m> {
        let n = m.cfg.n_layers;
        Session {
            m,
            keys: vec![Vec::new(); n],
            values: vec![Vec::new(); n],
            pos: 0,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Runs one token through the model and returns Q12 logits for the next one.
    pub fn step(&mut self, token: u32) -> Vec<i32> {
        self.steps(&[token]).pop().unwrap()
    }

    /// Runs consecutive tokens at once; result `i` is what `step` would return for
    /// `tokens[i]`. Every token's arithmetic is independent of the batch, so the
    /// logits are identical, but weights are read once per batch instead of per token.
    pub fn steps(&mut self, tokens: &[u32]) -> Vec<Vec<i32>> {
        let m = self.m;
        assert!(self.pos + tokens.len() <= m.cfg.max_ctx, "context full");
        let e = &m.embed;
        let mut xs: Vec<Vec<i32>> = tokens
            .iter()
            .map(|&t| {
                let t = t as usize;
                e.q[t * e.cols..(t + 1) * e.cols]
                    .iter()
                    .map(|&q| sat32(round_shift(q as i64 * e.mant[t], e.shift[t] - ACT_FRAC)))
                    .collect()
            })
            .collect();

        for l in 0..m.cfg.n_layers {
            let a = self.attention(l, &xs);
            xs.iter_mut().zip(&a).for_each(|(x, a)| add(x, a));
            let f = self.mlp(l, &xs);
            xs.iter_mut().zip(&f).for_each(|(x, f)| add(x, f));
        }
        self.pos += tokens.len();
        let hs: Vec<QVec> = xs.iter().map(|x| rmsnorm(x, &m.final_norm)).collect();
        linear(&hs, &m.embed)
    }

    fn rope(&self, v: &mut [i32], pos: usize) {
        let half = self.m.cfg.head_dim / 2;
        let cos = &self.m.rope_cos[pos * half..(pos + 1) * half];
        let sin = &self.m.rope_sin[pos * half..(pos + 1) * half];
        for head in v.chunks_mut(self.m.cfg.head_dim) {
            for j in 0..half {
                let (a, b) = (head[j] as i64, head[j + half] as i64);
                let (c, s) = (cos[j] as i64, sin[j] as i64);
                head[j] = sat32(round_shift(a * c - b * s, 30));
                head[j + half] = sat32(round_shift(b * c + a * s, 30));
            }
        }
    }

    fn attention(&mut self, l: usize, xs: &[Vec<i32>]) -> Vec<Vec<i32>> {
        let m = self.m;
        let (d, hd, kvd) = (
            m.cfg.d_model,
            m.cfg.head_dim,
            m.cfg.n_kv_heads * m.cfg.head_dim,
        );
        let layer = &m.layers[l];
        let hs: Vec<QVec> = xs.iter().map(|x| rmsnorm(x, &layer.attn_norm)).collect();
        let mut qkvs = linear(&hs, &layer.qkv);
        for (i, qkv) in qkvs.iter_mut().enumerate() {
            self.rope(&mut qkv[..d + kvd], self.pos + i);
            self.keys[l].extend_from_slice(&qkv[d..d + kvd]);
            self.values[l].extend_from_slice(&qkv[d + kvd..]);
        }

        let (keys, values) = (&self.keys[l], &self.values[l]);
        let (heads, group) = (m.cfg.n_heads, m.cfg.n_heads / m.cfg.n_kv_heads);
        // softmax(q.k / sqrt(64)) . v per token and head, result in Q28; token i sees
        // only the positions up to its own
        let outs: Vec<Vec<i64>> = (0..xs.len() * heads)
            .into_par_iter()
            .map(|job| {
                let (i, hi) = (job / heads, job % heads);
                let n = self.pos + i + 1;
                let qh = &qkvs[i][hi * hd..(hi + 1) * hd];
                let off = (hi / group) * hd;
                let scores: Vec<i64> = (0..n)
                    .map(|j| {
                        let kj = &keys[j * kvd + off..j * kvd + off + hd];
                        let d: i128 = qh
                            .iter()
                            .zip(kj)
                            .map(|(&a, &b)| a as i128 * b as i128)
                            .sum();
                        round_shift128(d, ACT_FRAC + 3) as i64
                    })
                    .collect();
                let max = *scores.iter().max().unwrap();
                let w: Vec<i64> = scores.iter().map(|&s| exp_neg(m, max - s)).collect();
                let den: i128 = w.iter().map(|&w| w as i128).sum();
                let inv = (1i128 << 62) / den;
                let mut num = vec![0i128; hd];
                for (j, &wj) in w.iter().enumerate() {
                    let vj = &values[j * kvd + off..j * kvd + off + hd];
                    for (acc, &v) in num.iter_mut().zip(vj) {
                        *acc += wj as i128 * v as i128;
                    }
                }
                // num/den is Q12; times inv (2^62/den) is Q74
                num.iter()
                    .map(|&s| round_shift128(s * inv, 46) as i64)
                    .collect()
            })
            .collect();
        let qs: Vec<QVec> = outs
            .chunks(heads)
            .map(|h| quantize(&h.concat(), 28))
            .collect();
        linear(&qs, &layer.o)
    }

    fn mlp(&self, l: usize, xs: &[Vec<i32>]) -> Vec<Vec<i32>> {
        let layer = &self.m.layers[l];
        let hs: Vec<QVec> = xs.iter().map(|x| rmsnorm(x, &layer.mlp_norm)).collect();
        let acts: Vec<QVec> = linear(&hs, &layer.gate_up)
            .iter()
            .map(|gu| {
                let (g, u) = gu.split_at(self.m.cfg.d_ff);
                let act: Vec<i64> = g
                    .iter()
                    .zip(u)
                    .map(|(&g, &u)| silu(self.m, g) * u as i64)
                    .collect();
                quantize(&act, 2 * ACT_FRAC)
            })
            .collect();
        linear(&acts, &layer.down)
    }
}

fn add(x: &mut [i32], y: &[i32]) {
    for (a, &b) in x.iter_mut().zip(y) {
        *a = sat32(*a as i64 + b as i64);
    }
}

/// Cumulative frequencies for the range coder: token t spans `cum[t]..cum[t+1]`,
/// every token gets at least 1, and the total never exceeds `FREQ_TOTAL`.
pub fn cumulative(m: &Model, logits: &[i32]) -> Vec<u32> {
    let max = *logits.iter().max().unwrap() as i64;
    let p: Vec<u64> = logits
        .iter()
        .map(|&l| exp_neg(m, max - l as i64) as u64)
        .collect();
    let sum: u64 = p.iter().sum();
    let spare = FREQ_TOTAL - logits.len() as u64;
    let mut cum = Vec::with_capacity(logits.len() + 1);
    let mut c = 0u32;
    cum.push(0);
    for &p in &p {
        c += (p * spare / sum) as u32 + 1;
        cum.push(c);
    }
    cum
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn dot_kernels_agree_on_extremes() {
        for n in [16usize, 256, 272, 576, 1536] {
            for (x, w) in [
                (32767i16, 127i8),
                (32767, -128),
                (-32767, -128),
                (-32767, 127),
            ] {
                let (a, q) = (vec![x; n], vec![w; n]);
                let want = n as i64 * x as i64 * w as i64;
                assert_eq!(dot_scalar(&a, &q), want);
                assert_eq!(dot(&a, &q), want);
            }
        }
    }

    // any change to a rounding rule, table or the frequency formula changes this hash, and
    // changes what goes on air; the same digest comes from an implementation of docs/engine.md
    // written separately in Python
    #[test]
    fn shipped_model_frequencies_are_pinned() {
        let m = crate::test_model();
        let inputs = [
            0u32, 26524, 216, 36, 32, 2199, 411, 216, 33, 38, 32, 32, 60, 28, 273, 313,
        ];
        let hash = |logits: Vec<Vec<i32>>| {
            let mut h = Sha256::new();
            for l in logits {
                for c in cumulative(m, &l) {
                    h.update(c.to_le_bytes());
                }
            }
            h.finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let want = "06bb91abb7fe03b1816370286e2b83a7dd33804264ad730617deb000df90584e";

        let mut s = Session::new(m);
        assert_eq!(
            hash(inputs.iter().map(|&t| s.step(t)).collect()),
            want,
            "one at a time"
        );
        let mut s = Session::new(m);
        let mut batched = s.steps(&inputs[..5]);
        batched.extend(s.steps(&inputs[5..]));
        assert_eq!(hash(batched), want, "in batches of 5 and 11");
    }
}
