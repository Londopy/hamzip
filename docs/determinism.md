# Checking that machines agree

hamzip only works if the sender and receiver compute exactly the same token frequencies. Integer arithmetic guarantees that on paper; these checks show it in practice.

## How to check a machine

```bash
python tools/mix.py mix --count 2000
hamzip bench mix                    # add --encode-only for a quick digest on a slow machine
```

`mix.py` writes the same synthetic messages on every machine (lines spliced from the corpus with a fixed seed). `bench` encodes and decodes each one, fails if any message doesn't round-trip, and prints a SHA-256 over every model-coded message. Equal digests mean two machines produce identical model-coded bytes, so each can decode the other's messages. Messages that went out as zstd or raw are left out of the digest, because zstd's output may change with its version without affecting anyone's ability to decode it.

| Model | Message set | Digest |
|---|---|---|
| smollm2-135m | `--count 2000` | `c945be8d4212896dd07f688dd80cd7115e6b344c2111db4430e47d7a50a1872b` |
| smollm2-135m | `--count 100` (the first 100 of the same set) | `43ff66700a0f95a8aedbffe44022f964d01a8555d99ecffbefd885dbebefef7c` |
| smollm2-360m | `--count 100` | `9b534c26aa5f97f1c037c229ffd7729cf047d1f3cd5acb30c363b5c8355f072e` |

These are for message format 1; run `bench` with `--model smollm2-360m` for the last row. `cargo test` also runs the golden vectors in `tests/golden/` and a pinned hash of the frequency tables. The [Determinism workflow](../.github/workflows/determinism.yml) repeats all of this on GitHub for every push to main and once a week.

## Results

| Machine | Dot product | Tests and golden vectors | 100 messages | 2000 messages |
|---|---|---|---|---|
| Windows 11, x86-64 (Core Ultra 9), the reference | AVX2 | pass | round trip | round trip |
| Windows 11, the published 32-bit `hamzip-v0.1.0-x86-portable.exe` | plain Rust | golden vectors exact | round trip, same digest | not run |
| GitHub `ubuntu-24.04-arm`, native aarch64 | plain Rust (NEON) | pass | round trip, same digest | round trip, same digest |
| GitHub `ubuntu-24.04`, x86-64 Linux | AVX2 | pass | round trip, same digest | encoded, same digest |
| GitHub `ubuntu-24.04`, armv7 32-bit under QEMU | plain Rust | library and golden tests pass | encoded, same digest | not run |

The aarch64 run is the one that matters most for a Raspberry Pi: it decodes all 2000 messages on real ARM hardware, from encodings bit-identical to the x86 reference, so a Pi decodes what a Windows PC sends. It also ran faster than expected, about 190 tokens a second encoding and 130 decoding on a 4-core runner. That is a server-class ARM core, though, not a Pi. The armv7 job runs under emulation at about 3 tokens a second, which is why it only encodes the 100-message set; its golden-vector test decodes under QEMU. The CLI and upgrade tests start programs themselves, outside cargo's QEMU runner, so that job runs only the library tests and golden vectors; the others test the command line and downloads, not the arithmetic.

Messages from the earlier local runs under Docker's QEMU (aarch64 and armv7, before the shared context was added) also matched x86 exactly.

## Not checked yet

- smollm2-360m anywhere but the Windows dev machine, where all 100 messages round-trip; the Determinism workflow adds it on ARM and x86 Linux.
- A real Raspberry Pi, including its speed. The native aarch64 runner shows the arithmetic agrees on ARM; a Pi's slower cores and memory will decode more slowly.
- The Windows arm64 build.
- A message sent over the air, for example over VARA FM, timed with and without hamzip.
