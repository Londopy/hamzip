# Checking that machines agree

hamzip only works if the sender and receiver compute exactly the same token frequencies. Integer arithmetic guarantees that on paper; these checks show it in practice.

## How to check a machine

```bash
python tools/mix.py mix --count 2000
hamzip bench mix
```

`mix.py` writes the same synthetic messages on every machine (lines spliced from the corpus with a fixed seed). `bench` encodes and decodes each one, fails if any message doesn't round-trip, and prints a SHA-256 over every encoded message. Equal digests mean two machines produce identical bytes, so each can decode the other's messages.

| Message set | Digest |
|---|---|
| `--count 2000` | `2cc8ce4f6e2b70b79c13feed6138d7c648c1b08f1188c21623951c9fe4473a57` |
| `--count 100` (the first 100 of the same set) | `38a2ccabf0527600069388ee8194007c16f69d4670c42936e6a871210e102942` |

These are for model `smollm2-135m.hzm` (SHA-256 `4f3b006a...c919`) and message format 1. `cargo test` also runs the golden vectors in `tests/golden/` and a pinned hash of the frequency tables.

## Results so far

| Machine | Dot product | Golden vectors and unit tests | 100 messages | 2000 messages |
|---|---|---|---|---|
| Windows 11, x86-64 (Core Ultra 9) | AVX2 | pass | same digest | same digest, all round-trip |
| Linux aarch64, emulated with QEMU | plain Rust (NEON) | pass | same digest | not run |
| Linux armv7, 32-bit, emulated with QEMU | plain Rust | pass | same digest | not run |

The ARM builds were cross-compiled with the release toolchain and run in Docker's QEMU emulation, which is too slow (about 7 tokens a second) for the 2000-message set. The x86 runs take the AVX2 path and the ARM runs take the plain one, so the two dot product implementations agree as well. On ARM the network tests need the system's CA certificates installed (`ca-certificates` on Debian), as any TLS client does.

## Not checked yet

- Real Raspberry Pi hardware, including speed. Emulation shows the arithmetic agrees, not how fast a Pi decodes.
- The 2000-message set on ARM.
- A message sent over the air, for example over VARA FM, timed with and without hamzip.
