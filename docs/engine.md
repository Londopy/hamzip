# Integer engine

The engine runs a Llama model using integer arithmetic only, with the rounding rules below. Integer addition and multiplication give the same answer on every CPU, in any order and on any number of threads, so any implementation that follows this page produces exactly the same token frequencies as `src/engine.rs`. One frequency off by one is enough to wreck everything the decoder outputs after it, so details like rounding direction matter.

The model file, its tables and the meaning of each section are in [model-format.md](model-format.md).

Changing anything on this page changes the bits on air, even with the same model file. Such a change needs a new message format version, and the frequency hash pinned in `src/engine.rs` will change.

## Notation

- `Qn` means fixed point with n fraction bits: the integer `v` stands for `v / 2^n`.
- `rshift(x, s)` is `floor((x + 2^(s-1)) / 2^s)` for `s > 0` (an arithmetic shift right with round half up), and `x * 2^-s` for `s <= 0`. It is computed in i128 and the result clamped to the i64 range.
- `sat32(x)` clamps to the i32 range.
- Products and sums are exact: use i64 or i128 where the comments say so. Nothing on this page may wrap.

The loader refuses model files outside the limits these bounds rely on: row shifts above 47, matrices wider than 65536, norm gains above 16, rope entries above 1.0, an exp table that doesn't start at 2^30 or ever increases, and a vocabulary of 2^23 or more.

The residual stream and the outputs of every matrix multiply are i32 in Q12.

## Matrix multiply

The input vector is first reduced to 16 bits, one shift for the whole vector:

```
quantize(v[], frac):            v is i64 at Q(frac)
    s = max(0, bitlen(max |v|) - 15)
    a[i] = clamp(rshift(v[i], s), -32767, 32767)      i16
    return a, frac - s
```

Then for each output row `r` of an int8 matrix with mantissa `m[r]` and shift `e[r]`:

```
acc = sum over i of a[i] * q[r][i]        exact; i64 total, i32 within blocks of 256
out[r] = sat32(rshift(acc * m[r], e[r] + frac - 12))      Q12
```

## RMSNorm

```
ss   = sum x[i]^2                       i128, Q24
mean = floor(ss / d) + 168              168 is 1e-5 in Q24
rms  = isqrt(mean * 2^16)               floor square root, Q20
inv  = floor(2^62 / rms)                Q42
y[i] = rshift(x[i] * gain[i] * inv, 42) i128 product, result Q28
return quantize(y, 28)
```

## Tables

The silu and exp tables are sampled every 1/64, which is 64 steps in Q12.

```
silu(x):                     x in Q12, result Q12
    if x >= 16 * 2^12: return x
    if x < -16 * 2^12: return 0
    i = x + 16 * 2^12, k = i >> 6, f = i & 63
    return T[k] + floor(((T[k+1] - T[k]) * f + 32) / 64)

exp_neg(d):                  d >= 0 in Q12, result Q30
    if d >= 24 * 2^12: return 0
    k = d >> 6, f = d & 63
    return T[k] - floor(((T[k] - T[k+1]) * f + 32) / 64)
```

The divisions by 64 are arithmetic shifts (round toward minus infinity).

## One step

Input: a token id `t` and its position `p` (0 for the start token).

1. Embedding: `x[i] = sat32(rshift(q[t][i] * m[t], e[t] - 12))` from the `embed` matrix.
2. For each layer:
   - `h = rmsnorm(x, attn_norm)`, then `q, k, v` = the q, k and v matrix multiplies of `h`.
   - Rotary embedding on every head of `q` and `k`, pairing element `j` with `j + 32`, with `c = rope_cos[p][j]` and `s = rope_sin[p][j]`:
     `a' = sat32(rshift(a*c - b*s, 30))`, `b' = sat32(rshift(b*c + a*s, 30))`.
   - Append `k` and `v` to this layer's cache.
   - For each query head (heads `3g..3g+2` share key/value head `g`):
     - `score[j] = rshift(dot(q_head, k_j), 15)` for every cached position `j <= p`. The dot product is i128 at Q24; the shift returns it to Q12 and divides by sqrt(64) = 8.
     - `w[j] = exp_neg(max(score) - score[j])`, `den = sum w[j]`, `inv = floor(2^62 / den)` (all i128).
     - `out[i] = rshift((sum over j of w[j] * v_j[i]) * inv, 46)`, i128, result Q28.
   - Concatenate the heads, `quantize(.., 28)`, multiply by `o`, add to `x` with `sat32`.
   - `h = rmsnorm(x, mlp_norm)`, then `g` and `u` = the gate and up multiplies of `h`.
   - `a[i] = silu(g[i]) * u[i]` (i64, Q24), `quantize(a, 24)`, multiply by `down`, add to `x` with `sat32`.
3. `logits` = `rmsnorm(x, final_norm)` multiplied by `embed` (the embedding matrix is the output layer too).

## Frequencies

```
M    = max logits
p[t] = exp_neg(M - logits[t])           Q30
S    = sum p[t]                         u64
f[t] = floor(p[t] * (2^24 - V) / S) + 1     V = vocab size, u64 product
```

The total `sum f[t]` never exceeds 2^24. Token `t` covers `[sum of f below t, plus f[t])` in token-id order.
