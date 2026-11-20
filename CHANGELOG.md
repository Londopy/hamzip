# Changelog

All notable changes to hamzip are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The section for a version is what appears on its GitHub release page. Validated in CI by [patchnotes](https://github.com/Londopy/patchnotes).

## [Unreleased]

## [0.1.0] - 2026-11-19

### Added

- An integer-only inference engine for SmolLM2-135M, so every CPU computes the same token probabilities, bit for bit.
- A 64-bit range coder that turns those probabilities into the bits on air.
- Message format version 1: each message goes out model-coded, as zstd or as plain text, whichever is smallest, with a CRC-32 of the text and the model's id.
- `hamzip encode`, `hamzip decode` and `hamzip bench`.
- `hamzip model download` fetches the shared model (137 MB) and keeps it only if its SHA-256 matches.
- `hamzip upgrade` installs the latest release after checking it against `SHA256SUMS.txt`, and a once-a-day notice says when there is one.
- Golden vectors: fixed messages and the exact bytes they must encode to, checked by `cargo test`.
- Downloads for Windows (installer, MSIs, portable exes and zip, `install.ps1`) and for Linux on x86_64, aarch64 and armv7 (Raspberry Pi), with `SHA256SUMS.txt` and torrents.

[Unreleased]: https://github.com/Londopy/hamzip/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Londopy/hamzip/releases/tag/v0.1.0
