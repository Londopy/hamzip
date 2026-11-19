"""Compare classic compressors with the ideal code length of a language model on the corpus."""

import argparse
import hashlib
import json
import math
import statistics
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

import torch
import zstandard as zstd
from transformers import AutoModelForCausalLM, AutoTokenizer

ROOT = Path(__file__).resolve().parent.parent

# what both sides would agree to prepend; costs nothing on air
PREAMBLES = {
    "none": "",
    "generic": "Amateur radio message sent over Winlink:\n\n",
}


def load_corpus():
    msgs = []
    for path in sorted((ROOT / "corpus").glob("*.jsonl")):
        with path.open(encoding="utf-8") as f:
            msgs.extend(json.loads(line) for line in f if line.strip())
    return msgs


def is_train(msg):
    # stable split by id so dictionaries never see the test messages
    return hashlib.sha256(msg["id"].encode()).digest()[0] % 2 == 0


def deflate(text, zdict=None):
    kw = {"zdict": zdict} if zdict else {}
    c = zlib.compressobj(9, zlib.DEFLATED, -15, **kw)
    return len(c.compress(text.encode()) + c.flush()) * 8


def zstd_compressor(dict_data=None):
    # magicless frame with no size, checksum or dict id: the smallest framing zstd allows
    params = zstd.ZstdCompressionParameters.from_level(
        19,
        format=zstd.FORMAT_ZSTD1_MAGICLESS,
        write_content_size=False,
        write_checksum=False,
        write_dict_id=False,
    )
    return zstd.ZstdCompressor(compression_params=params, dict_data=dict_data)


def baselines(train, test):
    train_bytes = [m["text"].encode() for m in train]
    zdict = b"".join(train_bytes)[-32768:]
    zs = {"zstd-19": zstd_compressor()}
    for size in (4096, 16384):
        d = zstd.train_dictionary(size, train_bytes)
        zs[f"zstd-19 + {size // 1024}K dict"] = zstd_compressor(d)

    out = {
        "deflate-9": [deflate(m["text"]) for m in test],
        "deflate-9 + 32K zdict": [deflate(m["text"], zdict) for m in test],
    }
    for name, c in zs.items():
        out[name] = [len(c.compress(m["text"].encode())) * 8 for m in test]
    return out


@torch.inference_mode()
def model_bits(model, tok, text, preamble, quant_total):
    """Ideal arithmetic-coding cost of text plus an end-of-message token, in bits.

    Returns (float cost, cost after rounding probabilities to integer frequencies
    summing to quant_total with a floor of 1, which is what the real coder will do).
    """
    eos = tok.eos_token_id
    ctx = [eos] + (tok.encode(preamble) if preamble else [])
    body = tok.encode(text) + [eos]
    ids = torch.tensor([ctx + body])
    logp = torch.log_softmax(model(ids).logits[0, len(ctx) - 1 : -1].double(), dim=-1)
    target = torch.tensor(body)
    exact = -logp.gather(1, target[:, None]).sum().item() / math.log(2)

    vocab = logp.shape[1]
    freq = torch.floor(logp.exp() * (quant_total - vocab)) + 1
    q = freq.gather(1, target[:, None])[:, 0] / freq.sum(dim=1)
    quant = -torch.log2(q).sum().item()
    return exact, quant, len(body)


def model_costs(name, test, preamble):
    path = ROOT / "models" / name
    tok = AutoTokenizer.from_pretrained(path)
    model = AutoModelForCausalLM.from_pretrained(path, dtype=torch.float32).eval()
    exact, quant, ntok = [], [], 0
    t0 = time.perf_counter()
    for m in test:
        e, q, n = model_bits(model, tok, m["text"], preamble, 1 << 24)
        exact.append(e)
        quant.append(q)
        ntok += n
    rate = ntok / (time.perf_counter() - t0)
    return exact, quant, ntok, rate


def bpc(bits, test):
    return sum(bits) / sum(len(m["text"]) for m in test)


def row(label, bits, test, header_bytes):
    per_msg = [b / len(m["text"]) for b, m in zip(bits, test)]
    with_hdr = [(b + header_bytes * 8) / len(m["text"]) for b, m in zip(bits, test)]
    short = [b / len(m["text"]) for b, m in zip(bits, test) if 100 <= len(m["text"]) <= 500]
    return (
        f"| {label} | {bpc(bits, test):.2f} | {statistics.median(per_msg):.2f} "
        f"| {statistics.median(short):.2f} | {statistics.median(with_hdr):.2f} |"
    )


def hamzip_run(exe, model, test):
    """Round-trip the test split through the real codec; returns payload bits and its report."""
    with tempfile.TemporaryDirectory() as d:
        for m in test:
            (Path(d) / f"{m['id']}.txt").write_bytes(m["text"].encode())
        r = subprocess.run([exe, "-m", model, "bench", d], capture_output=True, text=True, timeout=7200)
    if r.returncode != 0:
        sys.exit(f"hamzip bench failed: {r.stderr.strip()}")
    rows = {}
    for line in r.stdout.splitlines()[1:]:
        f = line.split("\t")
        if len(f) == 7:
            rows[f[0].removesuffix(".txt")] = {"tokens": int(f[2]), "bytes": int(f[3]), "mode": f[4]}
    # model mode frames are 1 header byte, 2 model id bytes and a 4 byte crc; the others skip the id
    bits = [(rows[m["id"]]["bytes"] - (7 if rows[m["id"]]["mode"] == "Model" else 5)) * 8 for m in test]
    summary = [line for line in r.stdout.splitlines() if line and "\t" not in line]
    return bits, rows, summary


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--models", nargs="*", default=["SmolLM2-135M", "SmolLM2-360M"])
    ap.add_argument("--header", type=int, default=7, help="bytes of hamzip framing to charge per message")
    ap.add_argument("--hamzip", help="path to the hamzip binary, to measure the real codec too")
    ap.add_argument("--hamzip-model", default=str(ROOT / "models" / "smollm2-135m.hzm"))
    args = ap.parse_args()

    msgs = load_corpus()
    train = [m for m in msgs if is_train(m)]
    test = [m for m in msgs if not is_train(m)]
    lengths = [len(m["text"]) for m in test]
    print(f"corpus: {len(msgs)} messages, {len(train)} train / {len(test)} test")
    print(f"test lengths: min {min(lengths)}, median {statistics.median(lengths)}, max {max(lengths)}")
    print(f"header charged: {args.header} bytes (baselines get the same charge)\n")
    print("| method | bpc (total) | bpc (median msg) | median, 100-500 chars | median incl. header |")
    print("|---|---|---|---|---|")
    for label, bits in baselines(train, test).items():
        print(row(label, bits, test, args.header))
    speeds = []
    for name in args.models:
        for pname, preamble in PREAMBLES.items():
            exact, quant, ntok, rate = model_costs(name, test, preamble)
            print(row(f"{name}, preamble {pname}", exact, test, args.header))
            print(row(f"{name}, preamble {pname}, 24-bit freqs", quant, test, args.header))
            speeds.append(f"{name} preamble {pname}: {ntok} tokens, {rate:.0f} tok/s")
    if args.hamzip:
        bits, rows, summary = hamzip_run(args.hamzip, args.hamzip_model, test)
        print(row("hamzip (integer engine, actual bytes)", bits, test, args.header))
        tok = AutoTokenizer.from_pretrained(ROOT / "models" / "SmolLM2-135M")
        differ = [m["id"] for m in test if len(tok.encode(m["text"])) != rows[m["id"]]["tokens"]]
        modes = sorted({r["mode"] for r in rows.values()})
        print(f"\nhamzip: {' '.join(summary)}")
        print(f"modes used: {modes}; token count differs from Hugging Face on {len(differ)} messages {differ[:5]}")
    if speeds:
        print("\nfloat32 cpu, whole message per forward pass:")
        print("\n".join(speeds))


if __name__ == "__main__":
    main()
