# Model file format, version 1

A hamzip model file holds everything a station needs to code messages: the tokenizer, int8 weights, and the lookup tables the integer engine uses in place of floating-point math. Two stations can talk only if their model files are byte-identical, which the wire header checks with a prefix of the file's SHA-256.

`tools/convert.py build` produces the file from a Hugging Face Llama checkpoint with tied embeddings. Rebuilding on the same machine gives the same bytes, but the tables are computed with the platform's float64 `exp`, `cos` and `log2`, so a rebuild elsewhere is not guaranteed to match. Distribute the published file and check its hash; don't rebuild it.

Published files:

| File | Source | SHA-256 |
|---|---|---|
| `smollm2-135m.hzm` | HuggingFaceTB/SmolLM2-135M (Apache-2.0) | `4f3b006a37c45d5f7ac496eba7bae7deef47661feeed1891b3340e18b4b9c919` |

## Layout

All integers are little-endian.

```
magic      8 bytes   "HAMZIPML"
version    u32       1
count      u32       number of sections
section    repeated count times:
  name_len u16
  name     name_len bytes, ASCII
  dtype    u32       0 = u8, 1 = i8, 2 = i32, 3 = u32
  ndim     u32
  shape    ndim x u32
  data     product(shape) elements, row-major
```

There is no padding and nothing follows the last section. Readers look sections up by name; their order is not significant.

## Sections

`config`, u32[9]: `n_layers, d_model, n_heads, n_kv_heads, head_dim, d_ff, vocab, max_ctx, eos_id`. For SmolLM2-135M that is `30, 576, 9, 3, 64, 1536, 49152, 2048, 0`.

### Tokenizer

| Name | Type | Meaning |
|---|---|---|
| `tok.offsets` | u32[vocab + 1] | token `i` is `tok.bytes[offsets[i]..offsets[i+1]]` |
| `tok.bytes` | u8[] | raw bytes of every token, concatenated |
| `tok.merges` | u32[n, 3] | BPE merges in priority order: `left, right, result` token ids |

Special tokens have zero-length bytes. The encoder never emits one except `eos_id`, which ends every message.

Decoding needs only the token bytes, plus the ids of the shared context, which [wire-format.md](wire-format.md) lists. The merges and the pre-tokenizer (digits split one at a time, then the GPT-2 pattern) are for the encoder, and for turning the shared context into ids. A message's own text can be split differently and still decode exactly, just a little less compactly; the context ids, by contrast, must match the listed ones, or no message decodes.

### Weights

Every matrix is int8, row-major `[out, in]`, with one scale per row in a matching `.scale` section of u32. A scale packs a mantissa in the low 24 bits and a shift in the high 8 bits:

```
w_real[r][c] = q[r][c] * (scale[r] & 0xFFFFFF) * 2^-(scale[r] >> 24)
```

| Name | Shape |
|---|---|
| `embed`, `embed.scale` | `[vocab, d_model]`, also used as the output layer |
| `l{i}.q`, `l{i}.k`, `l{i}.v` | `[n_heads * head_dim, d_model]`, k and v use `n_kv_heads` |
| `l{i}.o` | `[d_model, n_heads * head_dim]` |
| `l{i}.gate`, `l{i}.up` | `[d_ff, d_model]` |
| `l{i}.down` | `[d_model, d_ff]` |
| `l{i}.attn_norm`, `l{i}.mlp_norm`, `final_norm` | i32 `[d_model]`, RMSNorm gain in Q16 (`value / 2^16`) |

Layers are numbered from 0. Rows of q and k are in Hugging Face order, so rotary embedding pairs element `j` with element `j + head_dim / 2`.

### Tables

Every table is sampled every 1/64, so in the engine's Q12 activations the index of `x` is `x >> 6` and the low 6 bits interpolate linearly to the next entry.

| Name | Type | Entries | Entry k |
|---|---|---|---|
| `tbl.silu` | i32 | 2049 | `round(silu(-16 + k/64) * 2^12)` |
| `tbl.exp` | u32 | 1537 | `round(exp(-k/64) * 2^30)` |
| `tbl.rope_cos` | i32 | `[max_ctx, head_dim/2]` | `round(cos(p * theta^(-2j/head_dim)) * 2^30)` |
| `tbl.rope_sin` | i32 | `[max_ctx, head_dim/2]` | same with `sin` |

Inputs outside a table's range clamp: silu of `x >= 16` is `x`, silu of `x < -16` is 0, and `exp(-d)` for `d >= 24` is 0. For SmolLM2 `theta` is 100000.
