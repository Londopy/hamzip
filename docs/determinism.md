# Checking that machines agree

hamzip only works if the sender and receiver compute exactly the same token frequencies. Integer arithmetic guarantees that on paper; these checks show it in practice.

## How to check a machine

```bash
python tools/mix.py mix --count 2000
hamzip bench mix                    # add --encode-only for a quick digest on a slow machine
```

`mix.py` writes the same synthetic messages on every machine (lines spliced from the corpus with a fixed seed). `bench` encodes and decodes each one, fails if any message doesn't round-trip, and prints a SHA-256 over every model-coded message. Equal digests mean two machines produce identical model-coded bytes, so each can decode the other's messages. Messages that went out as zstd or raw are left out of the digest, because zstd's output may change with its version without affecting anyone's ability to decode it.

| Message set | Digest |
|---|---|
| `--count 2000` | `c945be8d4212896dd07f688dd80cd7115e6b344c2111db4430e47d7a50a1872b` |
| `--count 100` (the first 100 of the same set) | `43ff66700a0f95a8aedbffe44022f964d01a8555d99ecffbefd885dbebefef7c` |

These are for model `smollm2-135m.hzm` (SHA-256 `4f3b006a...c919`) and message format 1. `cargo test` also runs the golden vectors in `tests/golden/` and a pinned hash of the frequency tables. The [Determinism workflow](../.github/workflows/determinism.yml) repeats all of this on GitHub for every push to main and once a week.

## Results

| Machine | Dot product | Tests and golden vectors | 100 messages | 2000 messages |
|---|---|---|---|---|
| Windows 11, x86-64 (Core Ultra 9), the reference | AVX2 | pass | round trip | round trip |

The rows for other machines are filled in from the Determinism workflow's runs.

## Not checked yet

- Real Raspberry Pi hardware, including speed.
- The Windows x86 (32-bit) and arm64 builds.
- A message sent over the air, for example over VARA FM, timed with and without hamzip.
