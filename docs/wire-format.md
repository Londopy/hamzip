# Message format, version 1

A hamzip message is the binary payload you send over VARA, ARDOP, packet or FreeDATA. It has no length field: the transport already delivers the payload's exact length.

```
header   1 byte    high nibble: format version (1); low nibble: mode
body     mode-specific
crc      4 bytes   CRC-32 (IEEE, as in zip and PNG) of the original text, little-endian
```

| Mode | Body |
|---|---|
| 0, model | 2 bytes: the first two bytes of the model file's SHA-256, then the range-coded tokens |
| 1, zstd | a zstd frame (level 19) without its 4-byte magic number, content size, checksum or dictionary id |
| 2, raw | the text as is |

The encoder tries all three and sends the smallest. Raw wins for very short text and zstd wins for text the model codes badly. A message in model mode costs 7 bytes of framing; the others cost 5. Decoders refuse zstd output over 1 MiB, so encoders send anything longer as raw.

A decoder that gets a model-mode message whose model id differs from its own model reports both ids and stops. It never prints text decoded with the wrong model.

## Model mode

The text is UTF-8. It is split into tokens with the model's byte-level BPE (see [model-format.md](model-format.md)), and the end-of-text token is appended. A message is limited to `max_ctx - 11` tokens, 2037 for SmolLM2-135M or roughly 7000 characters; longer text goes out as zstd or raw.

Before any message, both sides prime the model with a fixed start: the end-of-text token, then the shared context `"Amateur radio message sent over Winlink:\n\n"` in the model's tokens. For SmolLM2-135M those are `8112, 17919, 5171, 3714, 2362, 690, 16641, 8371, 42, 1116`, and a decoder must use exactly these ids. The context is never sent; it gives the model somewhere to start, which saves about 1% on average.

Exactly: run the model over the end-of-text token and the first nine context tokens (positions 0 to 9), discarding their logits. The tenth context token is the first coding input, at position 10, and its logits code the message's first token. Each later input is the previous message token. After each step the engine ([engine.md](engine.md)) turns the logits into integer frequencies `f[t] >= 1` with a total of at most 2^24, and the range coder codes the actual next token with them. The decoder runs the same steps, so it sees the same frequencies, and stops at the end-of-text token.

## Range coder

The coder keeps a 64-bit `range` and a `low` with one extra carry bit, and works a byte at a time, like LZMA's range coder but 64 bits wide.

```
encode(start, size, total):
    r = range / total            (integer division)
    low += r * start
    range = r * size
    while range < 2^56: range <<= 8, shift_low()

shift_low():
    if (low mod 2^64) < 0xFF * 2^56 or low >= 2^64:
        carry = low >> 64
        emit cache + carry (except the very first time), then `pending` bytes of 0xFF + carry (mod 256)
        pending = 0, cache = (low >> 56) & 0xFF
    else:
        pending += 1
    low = (low mod 2^56) * 256
```

Start state: `low = 0`, `range = 2^64 - 1`, `cache = 0`, `pending = 0`. The first byte LZMA would emit is always zero, so it is dropped.

To finish, round `low` up to a multiple of 2^56 (that value is still inside the interval), call `shift_low` twice, and drop every trailing zero byte. The decoder treats bytes past the end as zero.

```
decoder start: code = first 8 bytes, big-endian; range = 2^64 - 1
target(total) = min(code / (range / total), total - 1)
    the symbol is the t with start[t] <= target < start[t] + size[t]
consume(start, size, total):
    r = range / total, code -= r * start, range = r * size
    while range < 2^56: range <<= 8, code = (code << 8) | next byte
```

A valid stream never makes the decoder read more than 8 bytes past its end, plus whatever zero bytes the encoder trimmed. The reference decoder gives up once it is 40 bytes past the end, so a corrupt message stops soon after its data runs out instead of running to the end of the context.
