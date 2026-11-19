## What this changes

<!-- One or two sentences. If it fixes an issue: "Fixes #123". -->

## Why

<!-- Skip if it is obvious from the above. -->

## AI tools

<!-- Required. "None", or which tool and what it did (drafted the change, wrote the tests, explained the code, wrote this description). See CONTRIBUTING.md. -->

## How I tested it

- [ ] `cargo test --release`, with the published model in `models/`
- [ ] Tried it in the real app (if user-facing)

## Bits on air

<!-- Tick one. See "The bits on air" in CONTRIBUTING.md. -->

- [ ] Nothing on air changes: the golden vectors and the pinned frequency hash pass untouched
- [ ] The bits on air change: new message format version, new golden vectors, updated docs/engine.md or docs/wire-format.md, and the new pinned hash if the frequencies changed

## Checklist

- [ ] `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] `CHANGELOG.md` entry under `[Unreleased]`, if this is user-visible
- [ ] The "AI tools" section above is filled in, and I have read and understood everything I am submitting
- [ ] I accept the [Contributor License Agreement](../CLA.md) (the CLA check will ask on a first pull request)
