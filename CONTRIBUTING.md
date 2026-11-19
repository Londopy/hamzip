# Contributing to hamzip

Issues and pull requests are welcome.

## Before you open a pull request

- Get the model once. The tests load `models/smollm2-135m.hzm` (or the file in
  `HAMZIP_MODEL`) and fail without it:
  `cargo build --release`, then `./target/release/hamzip model download --to models`.
  Use the published file rather than one you converted yourself: a model built on another
  machine can differ in a last bit.
- Lint: `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings`.
- Test: `cargo test`.
- Add a line under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) if the change is
  something a user would notice. The changelog is checked in CI by
  [patchnotes](https://github.com/Londopy/patchnotes).
- Fill in the pull request template, including the **AI tools** section.
- Sign the [CLA](CLA.md) when the check asks (once).

## The bits on air

Two stations can only read each other's messages if they compute exactly the same token
frequencies. One frequency off by one turns the rest of a message into garbage, so every
change falls into one of two kinds:

- **It changes the bits on air.** That is any change to the engine arithmetic (a rounding
  rule, a shift, a saturation), the lookup tables or the message format. It needs a new
  message format version (`FORMAT` in `src/lib.rs`), new golden vectors in `tests/golden/`,
  and the matching update to [docs/engine.md](docs/engine.md) or
  [docs/wire-format.md](docs/wire-format.md). A change to the arithmetic or the tables also
  changes the frequency hash pinned in `src/engine.rs`; update it in the same pull request.
- **It doesn't.** Everything else, speedups included, leaves the golden vectors and the
  pinned hash alone. If one of them fails, the change is wrong: never regenerate them to
  make a test pass.

The coding path uses integers only. Floating-point results can differ between CPUs,
compilers and math libraries, so floats stay out of the engine.

## Publishing the model (maintainers)

The model file lives on its own release, tagged `model-smollm2-135m`, which
`hamzip model download` fetches from. Upload the canonical file once, as a pre-release (or
with "Set as the latest release" unticked), so it never becomes the latest release that
`hamzip upgrade`, install.ps1 and the `releases/latest` links point at:

```bash
gh release create model-smollm2-135m models/smollm2-135m.hzm --prerelease \
  --title "Model: SmolLM2-135M" \
  --notes "SHA-256 4f3b006a37c45d5f7ac496eba7bae7deef47661feeed1891b3340e18b4b9c919"
```

Never replace that file. A different model gets a new file name, a new tag and its own
entry in `src/upgrade.rs` and [docs/model-format.md](docs/model-format.md).

## AI-assisted contributions

You may use AI tools to help write a contribution. Three rules, which the
CLA makes binding:

- **Say so in the pull request.** The template has a section for it: which
  tool, and what it did (drafted the change, wrote the tests, explained the
  code, translated the docs, wrote the description). "None" is a fine answer.
  A check on every pull request fails until the section is filled in.
- **You are the author.** Read and understand everything the tool produced
  before you submit it; you are answerable for it in review exactly as for
  code you typed. A pull request whose author cannot explain a change is
  sent back. Commits and files name people, not tools: an AI tool is never
  listed as an author or co-author, and commits carry no "Generated with"
  footers. The check rejects both.
- **No laundering.** Do not submit output that reproduces someone else's
  work under a license incompatible with this project's license, or that you
  have no right to license. If a tool hands you a recognizable chunk of
  another project, leave it out.

An undisclosed AI-assisted contribution is closed when discovered, whatever
its quality. Disclosed ones are reviewed like any other, and labeled
`ai-assisted`: the tests are the bar.

The same goes for issues: if an AI tool wrote or summarized a bug report,
say so in it.

## Contributor License Agreement

Your first pull request gets a comment from the CLA check asking you to
accept [`CLA.md`](CLA.md) by replying with one sentence. You keep the
copyright in your work; the agreement gives the project a license to
distribute it under its license (or another OSI-approved license, never a
proprietary one) and a patent license for what your contribution
necessarily uses, and it records that the work is yours to give,
AI-assisted or not. Every author of a commit in the pull request signs once,
and that covers all their later pull requests. Companies contributing on
behalf of employees can open an issue naming the covered accounts instead.
