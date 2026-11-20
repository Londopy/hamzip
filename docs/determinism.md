# Checking that machines agree

hamzip only works if the sender and receiver compute exactly the same token frequencies. Integer arithmetic guarantees that on paper; these checks show it in practice.

## How to check a machine

```bash
python tools/mix.py mix --count 2000
hamzip bench mix
```

`mix.py` writes the same synthetic messages on every machine (lines spliced from the corpus with a fixed seed). `bench` encodes and decodes each one, fails if any message doesn't round-trip, and prints a SHA-256 over every model-coded message. Equal digests mean two machines produce identical model-coded bytes, so each can decode the other's messages. Messages that went out as zstd or raw are left out of the digest, because zstd's output may change with its version without affecting anyone's ability to decode it.

| Message set | Digest |
|---|---|
| `--count 2000` | `9d539a2dc470bbbb9081e4ecec9df9042a7c3162f7fba381de8808b3b1d63178` |
| `--count 100` (the first 100 of the same set) | `b80b0fb3d217ac7ce2fbabc2b7e02cb528cc3d9e6f2a032f6fe56818c306f868` |

These are for model `smollm2-135m.hzm` (SHA-256 `4f3b006a...c919`) and message format 1. `cargo test` also runs the golden vectors in `tests/golden/` and a pinned hash of the frequency tables.

## Results so far

| Machine | Dot product | Golden vectors and unit tests | 100 messages | 2000 messages |
|---|---|---|---|---|
| Windows 11, x86-64 (Core Ultra 9) | AVX2 | pass | same digest | same digest, all round-trip |
| Linux aarch64, emulated with QEMU | plain Rust (NEON) | pass | same digest | not run |
| Linux armv7, 32-bit, emulated with QEMU | plain Rust | pass | same digest | not run |

The ARM builds were cross-compiled with the release toolchain and run in Docker's QEMU emulation, which is too slow (about 7 tokens a second) for the 2000-message set. Their 100-message runs were compared with an earlier version of the digest that hashed every message, zstd and raw included, which is a stricter test; it matched x86 exactly. The x86 runs take the AVX2 path and the ARM runs take the plain one, so the two dot product implementations agree as well. On ARM the network tests need the system's CA certificates installed (`ca-certificates` on Debian), as any TLS client does.

## Not checked yet

- Real Raspberry Pi hardware, including speed. Emulation shows the arithmetic agrees, not how fast a Pi decodes.
- The 2000-message set on ARM.
- The Windows x86 (32-bit) and arm64 builds.
- A message sent over the air, for example over VARA FM, timed with and without hamzip.
