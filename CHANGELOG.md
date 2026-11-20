# Changelog

All notable changes to hamzip are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The section for a version is what appears on its GitHub release page. Validated in CI by [patchnotes](https://github.com/Londopy/patchnotes).

## [Unreleased]

## [0.2.0] - 2026-11-19

### Added

- A second published model, smollm2-360m (365 MB): messages about 8% smaller on 100 to 500 character text, decoding at about half the speed. Get it with `hamzip model download smollm2-360m` and pick it with `--model smollm2-360m`.
- `--model` takes a published model's name as well as a path, and `hamzip model list` shows which models are installed.

### Changed

- `decode` and `info` pick whichever installed model a message was coded with, so a station can keep both models.
- A message for a model you don't have now names that model and says how to get it, instead of showing only its id.
- The README states the compression and speed limits plainly, says what hasn't been tested yet, and covers more of Part 97.

## [0.1.0] - 2026-11-19

### Added

- An integer-only inference engine for SmolLM2-135M, so every CPU computes the same token probabilities, bit for bit.
- A 64-bit range coder that turns those probabilities into the bits on air.
- Message format version 1: each message goes out model-coded, as zstd or as plain text, whichever is smallest, with a CRC-32 of the text and the model's id. A shared line of context, never sent, starts the model off before every message.
- `hamzip encode`, `hamzip decode`, `hamzip info` and `hamzip bench` (with `--encode-only` for a quick digest on a slow machine).
- `hamzip encode --text` writes a message as one line of `hz:` base64 for modes that only carry text; `decode` and `info` read it back.
- `hamzip model download` fetches the shared model (137 MB) and keeps it only if its SHA-256 matches.
- `hamzip upgrade` installs the latest release after checking it against `SHA256SUMS.txt`, and a once-a-day notice says when there is one.
- Golden vectors: fixed messages and the exact bytes they must encode to, checked by `cargo test`.
- Downloads for Windows (installer, MSIs, portable exes and zip, `install.ps1`) and for Linux on x86_64, aarch64 and armv7 (Raspberry Pi), with `SHA256SUMS.txt` and torrents.

[Unreleased]: https://github.com/Londopy/hamzip/compare/v0.2.0...HEAD
[0.1.0]: https://github.com/Londopy/hamzip/releases/tag/v0.1.0
[0.2.0]: https://github.com/Londopy/hamzip/compare/v0.1.0...v0.2.0
