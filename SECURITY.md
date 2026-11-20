# Security policy

hamzip decodes whatever arrives over the air, so it treats every received message as
hostile input.

## Reporting a vulnerability

**Do not open a public issue.** Report it through GitHub's private
vulnerability reporting on this repository (Security > Report a
vulnerability). If that isn't possible, email **Londopy@protonmail.com**.

If a message or a model file triggers the problem, attach it.

You'll get an acknowledgement within 72 hours, and an assessment within 7 days.

## Supported versions

| Version | Supported |
|---|---|
| Latest release | Yes |
| main | Yes |
| Older releases | No: `hamzip upgrade` moves you to the latest |

## Threat model

**In scope** (please report):

- A crafted message that makes `hamzip decode` crash, hang, or use memory out of all
  proportion to its size. Decoding is bounded on purpose: it stops at the end of the model's
  context or 40 bytes past the end of the message, and refuses zstd output over 1 MiB.
- A crafted model file that crashes or hangs hamzip instead of being refused when it loads.
- Decoded text that skips the checks the [message format](docs/wire-format.md) promises:
  text printed although it fails its CRC-32, or decoded with a model whose id doesn't match.
- A download accepted with the wrong hash: `hamzip model download` keeping a file whose
  SHA-256 isn't the one built into hamzip, or `hamzip upgrade` or `install.ps1` installing a
  file that is missing from the release's `SHA256SUMS.txt` or doesn't match it.
- `hamzip upgrade` replacing hamzip without asking (unless you passed `--yes`).
- Network traffic other than `hamzip upgrade`, `hamzip model download` and the once-a-day
  update check, which talk to GitHub only. The update check runs alongside other commands in
  a terminal; `HAMZIP_NO_UPDATE_CHECK=1` turns it off.

**Out of scope:**

- Reading or forging messages. hamzip is compression, not encryption: anyone with the
  published model can decode what they hear, by design. Nothing in a message proves who
  sent it, and the CRC-32 catches corruption, not tampering.
- Two different models sharing the 2-byte model id. The id catches the wrong model by
  accident; it doesn't authenticate one.
- A message that takes as long to decode as the longest legitimate one (2037 tokens). That
  is slow on a Raspberry Pi, but it is bounded.
- A damaged message failing to decode. There is no error correction on purpose; send
  hamzip messages over a link with ARQ.
- Vulnerabilities in dependencies such as zstd or rustls. Report those upstream, and open a
  private report here if hamzip needs a release because of one.

## Binaries

Release binaries and installers are not code-signed, so Windows SmartScreen may warn on
first run. Every release ships `SHA256SUMS.txt`; check a download against it before running
it:

```powershell
(Get-FileHash .\hamzip-0.1.0-setup-x64.exe -Algorithm SHA256).Hash.ToLower()
```

```sh
sha256sum -c SHA256SUMS.txt --ignore-missing
```

`SHA256SUMS.txt` sits on the same release as the files it lists, so it catches a damaged or
swapped download, not a compromised GitHub account. The model's SHA-256 is built into
hamzip itself.
