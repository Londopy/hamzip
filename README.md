# hamzip

[![CI](https://github.com/Londopy/hamzip/actions/workflows/ci.yml/badge.svg)](https://github.com/Londopy/hamzip/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Londopy/hamzip)](https://github.com/Londopy/hamzip/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/Londopy/hamzip/total)](https://github.com/Londopy/hamzip/releases)
[![changelog](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/Londopy/hamzip/gh-pages/changelog-badge.json)](CHANGELOG.md)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20Raspberry%20Pi-0078D6)](#install)

**Short text messages go over a slow radio link in about a third of the bits.**

Both stations keep the same small open language model. The sender uses it to predict each next word piece and spends very few bits on the likely ones; the receiver runs the same model and reverses it exactly. The result is an ordinary binary payload, so it rides on VARA, ARDOP, packet or FreeDATA without touching the modem.

```console
$ hamzip encode sitrep.txt -o sitrep.hz
$ hamzip decode sitrep.hz
SITREP Shelter 3, Loveland HS, 1400L. Occupants 142, staff 11. Power on generator,
fuel 1/2 tank. Water OK through tomorrow. Need: 40 cots, infant formula. Next report 1800L.
```

## Features

- A typical 100 to 500 character message comes out at about 2.6 bits per character, framing included. Plain text is 8; zstd with a dictionary trained on ham traffic gets about 5.
- The model runs on integer arithmetic only, so a Windows PC, a Linux laptop and a Raspberry Pi compute exactly the same probabilities. One different probability would turn the rest of the message into garbage, which is why no floating point is involved anywhere in the coding path.
- Every message carries a CRC-32 of the text and two bytes of the model's SHA-256. A station with a different model gets a clear error naming both, never wrong text.
- Short or odd text falls back to zstd or plain bytes automatically, whichever is smallest. One header byte says which.
- The format, the arithmetic and the model are published, so anyone can decode what they hear (see [On the air](#on-the-air)).

## Install

Every [release](https://github.com/Londopy/hamzip/releases/latest) has the same set of downloads.

| Download | What it is |
|---|---|
| `hamzip-<version>-setup-x64.exe` | **Installer (recommended).** Per-user, no admin prompt, adds `hamzip` to your PATH. |
| `hamzip-<version>-x64.msi` (also `x86`, `arm64`) | MSI installer. Per-user by default; can be deployed machine-wide. |
| `hamzip-v<version>-x64-portable.exe` (also `x86`, `arm64`) | One exe, nothing to install. |
| `hamzip-v<version>-portable.zip` | All three Windows builds in one zip; the model and data stay next to the exe. |
| `hamzip-v<version>-linux-aarch64` (also `armv7`, `x86_64`) | Linux binary. `aarch64` for 64-bit Raspberry Pi OS, `armv7` for 32-bit. |
| `install.ps1` / `install.cmd` | Script install that picks the right Windows build and checks its SHA-256. |
| `<file>.torrent` / magnet link | Each download above over BitTorrent; downloads straight from GitHub even with no seeders. |

**PowerShell one-liner:**

```powershell
irm https://github.com/Londopy/hamzip/releases/latest/download/install.ps1 | iex
```

**Raspberry Pi or other Linux** (use the current version number and `armv7` on 32-bit Raspberry Pi OS):

```bash
curl -LO https://github.com/Londopy/hamzip/releases/latest/download/hamzip-v0.1.0-linux-aarch64
chmod +x hamzip-v0.1.0-linux-aarch64 && sudo mv hamzip-v0.1.0-linux-aarch64 /usr/local/bin/hamzip
```

**Then get the model**, on every station:

```bash
hamzip model download
```

That fetches `smollm2-135m.hzm` (137 MB) and refuses to keep it unless its SHA-256 is `4f3b006a37c45d5f7ac496eba7bae7deef47661feeed1891b3340e18b4b9c919`. Do it once while you have internet; hamzip itself never needs a connection after that.

The binaries are not code-signed, so Windows SmartScreen may warn on first run. Every release ships `SHA256SUMS.txt`; check a download with `(Get-FileHash .\<file> -Algorithm SHA256).Hash` on Windows or `sha256sum -c SHA256SUMS.txt --ignore-missing` on Linux.

`hamzip upgrade` replaces hamzip with the latest release after checking it against `SHA256SUMS.txt`, and asks before it does. Once a day, when you run hamzip in a terminal with internet, it mentions a newer version on stderr; set `HAMZIP_NO_UPDATE_CHECK=1` to turn that off.

## Usage

```bash
hamzip encode message.txt -o message.hz     # or pipe: hamzip encode < message.txt > message.hz
hamzip decode message.hz                    # prints the text
hamzip decode message.hz -o message.txt
```

Send the `.hz` file as a binary payload or attachment with whatever you already use. The receiving station runs `hamzip decode` on it.

hamzip helps most where you control the payload byte for byte: FreeDATA, ARDOP or VARA sessions between your own programs, packet. Winlink also works, attaching the `.hz` file, but Winlink already compresses message bodies and adds its own headers, so the saving on a short Winlink message is smaller than the numbers below.

`hamzip bench <folder>` encodes and decodes every file in a folder and prints sizes, speed and a digest of all the output (see [Checking two stations agree](#checking-two-stations-agree)).

## How is this different from zip or zstd?

General-purpose compressors learn from the message itself, and a 300-character message doesn't give them much to learn from. hamzip brings what it knows about English from the model.

Measured on 132 held-out messages from the test corpus (Winlink-style email, ICS-213 forms, radiograms, net reports and SITREPs, written for this project), median message, framing included:

| Method | Bits per character | Size vs. plain text |
|---|---|---|
| Plain text | 8.00 | 100% |
| deflate (zip, gzip) | 6.22 | 78% |
| zstd -19 | 6.36 | 80% |
| zstd -19 with a 16 KB dictionary trained on ham traffic | 4.96 | 62% |
| **hamzip** | **2.61** | **33%** |

Ordinary email and ICS-213 text does best, around 1.8 to 2 bits per character before framing. Callsign lists, grid squares and numbers cost more, about 3 to 3.5, because nothing can predict a callsign it has never seen. The corpus is synthetic, so expect different numbers on your own traffic; `tools/bench.py` reproduces the table.

Speed on a Core Ultra 9 laptop is about 520 tokens a second encoding and 110 decoding, so a typical 300-character message takes about a second to decode. Decoding has to run one token at a time, so it is the slow side. On a Raspberry Pi it is much slower; measure yours with `hamzip bench`.

## On the air

In the US, Part 97 forbids messages encoded to obscure their meaning (97.113(a)(4)) and allows only publicly documented codes (97.309(a)(4)). hamzip is compression, not encryption, and everything needed to decode it is public: this repository, the model file and its hash, [the message format](docs/wire-format.md), [the model file format](docs/model-format.md) and [the exact arithmetic](docs/engine.md). Whether publishing a model file counts as public documentation hasn't been tested with the FCC; read the current rules on eCFR yourself, and check your own country's rules if you are not in the US. You still identify with your callsign in the clear as usual.

hamzip has no error correction on purpose. One wrong bit ruins everything after it, so only send hamzip messages over links that already guarantee delivery (ARQ). The CRC catches corruption; it can't repair it.

## Checking two stations agree

The golden vectors in `tests/golden/` are messages with the exact bytes they must encode to, and `cargo test` checks them. To check a new machine against yours with more messages:

```bash
python tools/mix.py mix --count 2000     # the same 2000 messages on every machine
hamzip bench mix                         # compare the digest line between machines
```

Equal digests mean both machines produce identical messages, so each can decode the other's. On this project's x86 build, the 2000-message set gives the digest printed in [docs/determinism.md](docs/determinism.md), along with the results from 64-bit and 32-bit ARM.

## Configuration

hamzip looks for the model in this order:

| Where | Notes |
|---|---|
| `--model <file>` or `HAMZIP_MODEL` | Any path. |
| `smollm2-135m.hzm` next to the hamzip executable | Handy for portable installs. |
| The data folder | `%LOCALAPPDATA%\hamzip` on Windows, `~/.local/share/hamzip` on Linux. `hamzip model download` saves here. A file named `portable` next to the exe makes the exe's folder the data folder. |

`HAMZIP_NO_UPDATE_CHECK=1` turns off the daily update notice; it is also off when `CI` is set or stderr isn't a terminal.

## Building from source

```bash
git clone https://github.com/Londopy/hamzip
cd hamzip
cargo build --release
./target/release/hamzip model download --to models
cargo test
```

The tests need the model in `models/` (or `HAMZIP_MODEL`); they fail rather than skip without it.

To build the model file yourself from the original weights:

```bash
cd tools
uv sync
uv run python -c "from huggingface_hub import snapshot_download as d; d('HuggingFaceTB/SmolLM2-135M', local_dir='../models/SmolLM2-135M')"
uv run python convert.py build ../models/SmolLM2-135M ../models/smollm2-135m.hzm
```

It should print the SHA-256 above. The lookup tables in the file are computed with your platform's floating-point math, so a build on a different machine can differ in a last bit; if your hash doesn't match, use the published file, because every station has to have the identical one.

Installers: [Inno Setup 6](https://jrsoftware.org/isinfo.php) builds the setup.exe (`ISCC.exe /DAppVersion=0.1.0 installer\hamzip.iss`) and [WiX v7](https://wixtoolset.org) builds the MSI (see `.github/workflows/release.yml` for the exact commands).

## How it works

1. The text is split into tokens with the model's byte-level BPE tokenizer.
2. For each token, the model gives a probability for every possible next token. Those probabilities are turned into integer frequencies, and a 64-bit range coder spends about `-log2(p)` bits on the token that actually comes next.
3. The receiver starts the model from the same state, so it gets the same frequencies, reads off one token at a time, and stops at the end-of-text token.

The model is [SmolLM2-135M](https://huggingface.co/HuggingFaceTB/SmolLM2-135M), converted to int8 weights with one scale per row, which costs nothing measurable in compression. Everything else in the forward pass (normalization, attention, softmax, SiLU, rotary position) is done in fixed point with lookup tables stored in the model file, so nothing depends on the CPU's floating-point unit or math library. Integer arithmetic gives the same answer in any order, which also makes multithreading and SIMD safe. The integer engine costs about 1% in compression against the same model in float32.

The full specification, enough to write a compatible decoder from scratch: [docs/engine.md](docs/engine.md), [docs/wire-format.md](docs/wire-format.md), [docs/model-format.md](docs/model-format.md).

## Layout

```
src/
  engine.rs      integer-only transformer forward pass
  rangecoder.rs  64-bit range coder
  tokenizer.rs   byte-level BPE encoder
  model.rs       model file loader and checks
  lib.rs         message format: model, zstd and raw modes
  main.rs        CLI
  upgrade.rs     model download, self-upgrade, update notice
tests/golden/    fixed messages and the bytes they must encode to
tools/           corpus benchmark, model converter, message mixer (Python)
corpus/          test messages written for this project
docs/            format and arithmetic specifications
installer/       Inno Setup and WiX scripts
scripts/         release_notes.py (release page, checksums, torrents)
```

## Contributing

Issues and pull requests are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md). Anything that changes the engine's arithmetic, its tables or the message format changes the bits on air and needs a new format version. Add a line under `Unreleased` in [CHANGELOG.md](CHANGELOG.md) with any change.

## License

hamzip is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The model file is derived from SmolLM2-135M by Hugging Face, which is licensed under Apache-2.0. The test corpus was written for this project; all names and callsigns in it are made up.
