"""Convert a Hugging Face Llama checkpoint into a hamzip model file, or check one.

The file layout and every constant here are specified in docs/model-format.md.
"""

import argparse
import hashlib
import json
import struct
import sys
from pathlib import Path

import numpy as np
import torch
from safetensors.torch import load_file

MAGIC = b"HAMZIPML"
VERSION = 1
ACT_FRAC = 12
NORM_FRAC = 16
TBL_FRAC = 30
TBL_STEP = 1 / 64
SILU_RANGE = 16
EXP_RANGE = 24

DTYPES = {"u8": (0, "<u1"), "i8": (1, "<i1"), "i32": (2, "<i4"), "u32": (3, "<u4")}
DTYPE_CODES = {code: (name, np.dtype(fmt)) for name, (code, fmt) in DTYPES.items()}
CONFIG_KEYS = ["n_layers", "d_model", "n_heads", "n_kv_heads", "head_dim", "d_ff", "vocab", "max_ctx", "eos_id"]
LINEARS = {
    "q": "self_attn.q_proj",
    "k": "self_attn.k_proj",
    "v": "self_attn.v_proj",
    "o": "self_attn.o_proj",
    "gate": "mlp.gate_proj",
    "up": "mlp.up_proj",
    "down": "mlp.down_proj",
}


def gpt2_byte_decoder():
    # inverse of the printable-unicode alias GPT-2 byte-level BPE uses for raw bytes
    bs = list(range(ord("!"), ord("~") + 1)) + list(range(ord("¡"), ord("¬") + 1)) + list(range(ord("®"), ord("ÿ") + 1))
    cs = bs[:]
    n = 0
    for b in range(256):
        if b not in bs:
            bs.append(b)
            cs.append(256 + n)
            n += 1
    return {chr(c): b for b, c in zip(bs, cs)}


def quantize_rows(w):
    """Symmetric int8 per row; scale packed as mantissa (low 24 bits) and shift (high 8 bits)."""
    w = w.double().numpy()
    s = np.abs(w).max(axis=1) / 127
    zero = s == 0
    s[zero] = 1
    e = 23 - np.floor(np.log2(s)).astype(np.int64)
    m = np.round(s * np.exp2(e)).astype(np.int64)
    over = m >= 1 << 24
    m[over] >>= 1
    e[over] -= 1
    # the engine refuses shifts above 47 so its products can't overflow
    if (e < 0).any() or (e > 47).any():
        sys.exit("row scale shift out of range")
    m[zero] = 0
    e[zero] = 0
    packed = (m | (e << 24)).astype("<u4")
    step = np.where(zero, 1, m * np.exp2(-e.astype(np.float64)))
    q = np.clip(np.round(w / step[:, None]), -127, 127).astype("<i1")
    return q, packed


def unpack_scales(packed):
    p = packed.astype(np.int64)
    return (p & 0xFFFFFF) * np.exp2(-(p >> 24).astype(np.float64))


def tables(cfg, theta):
    x = np.arange(-SILU_RANGE, SILU_RANGE + TBL_STEP / 2, TBL_STEP)
    silu = np.round(x / (1 + np.exp(-x)) * 2**ACT_FRAC).astype("<i4")
    d = np.arange(0, EXP_RANGE + TBL_STEP / 2, TBL_STEP)
    exp = np.round(np.exp(-d) * 2**TBL_FRAC).astype("<u4")
    half = cfg["head_dim"] // 2
    inv_freq = theta ** (-np.arange(half, dtype=np.float64) * 2 / cfg["head_dim"])
    ang = np.outer(np.arange(cfg["max_ctx"], dtype=np.float64), inv_freq)
    cos = np.round(np.cos(ang) * 2**TBL_FRAC).astype("<i4")
    sin = np.round(np.sin(ang) * 2**TBL_FRAC).astype("<i4")
    return {"tbl.silu": silu, "tbl.exp": exp, "tbl.rope_cos": cos, "tbl.rope_sin": sin}


def vocab_sections(hf_dir):
    tj = json.loads((hf_dir / "tokenizer.json").read_text(encoding="utf-8"))
    dec = gpt2_byte_decoder()
    vocab = tj["model"]["vocab"]
    special = {a["id"] for a in tj["added_tokens"] if a["special"]}
    pieces = [b""] * len(vocab)
    for s, i in vocab.items():
        if i not in special:
            pieces[i] = bytes(dec[c] for c in s)
    offsets = np.cumsum([0] + [len(p) for p in pieces]).astype("<u4")
    merges = []
    for m in tj["model"]["merges"]:
        a, b = m.split(" ") if isinstance(m, str) else m
        merges.append((vocab[a], vocab[b], vocab[a + b]))
    return {
        "tok.offsets": offsets,
        "tok.bytes": np.frombuffer(b"".join(pieces), dtype="<u1"),
        "tok.merges": np.array(merges, dtype="<u4"),
    }


def build(hf_dir, out, max_ctx):
    hc = json.loads((hf_dir / "config.json").read_text())
    if hc["model_type"] != "llama" or not hc.get("tie_word_embeddings"):
        sys.exit(f"{hf_dir}: only Llama models with tied embeddings are supported")
    cfg = {
        "n_layers": hc["num_hidden_layers"],
        "d_model": hc["hidden_size"],
        "n_heads": hc["num_attention_heads"],
        "n_kv_heads": hc["num_key_value_heads"],
        "head_dim": hc["hidden_size"] // hc["num_attention_heads"],
        "d_ff": hc["intermediate_size"],
        "vocab": hc["vocab_size"],
        "max_ctx": max_ctx,
        "eos_id": hc["eos_token_id"],
    }
    w = load_file(hf_dir / "model.safetensors")
    sec = {"config": np.array([cfg[k] for k in CONFIG_KEYS], dtype="<u4")}
    sec.update(vocab_sections(hf_dir))
    sec.update(tables(cfg, hc["rope_theta"]))

    def norm(name):
        return np.round(w[name].double().numpy() * 2**NORM_FRAC).astype("<i4")

    sec["embed"], sec["embed.scale"] = quantize_rows(w["model.embed_tokens.weight"])
    for i in range(cfg["n_layers"]):
        p = f"model.layers.{i}."
        sec[f"l{i}.attn_norm"] = norm(p + "input_layernorm.weight")
        sec[f"l{i}.mlp_norm"] = norm(p + "post_attention_layernorm.weight")
        for short, hf in LINEARS.items():
            sec[f"l{i}.{short}"], sec[f"l{i}.{short}.scale"] = quantize_rows(w[p + hf + ".weight"])
    sec["final_norm"] = norm("model.norm.weight")

    tmp = out.with_suffix(out.suffix + ".tmp")
    with tmp.open("wb") as f:
        f.write(MAGIC + struct.pack("<II", VERSION, len(sec)))
        for name, a in sec.items():
            code = DTYPES[{"u1": "u8", "i1": "i8", "i4": "i32", "u4": "u32"}[a.dtype.str[1:]]][0]
            f.write(struct.pack("<H", len(name)) + name.encode())
            f.write(struct.pack("<II", code, a.ndim) + struct.pack(f"<{a.ndim}I", *a.shape))
            f.write(a.astype(a.dtype.newbyteorder("<")).tobytes())
    tmp.replace(out)
    print(f"wrote {out} ({out.stat().st_size / 1e6:.1f} MB)")
    print(f"sha256 {hashlib.sha256(out.read_bytes()).hexdigest()}")


def read(path):
    data = path.read_bytes()
    if data[:8] != MAGIC:
        sys.exit(f"{path}: not a hamzip model file")
    version, n = struct.unpack_from("<II", data, 8)
    if version != VERSION:
        sys.exit(f"{path}: model format {version}, this script reads {VERSION}")
    pos, sec = 16, {}
    for _ in range(n):
        (ln,) = struct.unpack_from("<H", data, pos)
        name = data[pos + 2 : pos + 2 + ln].decode()
        pos += 2 + ln
        code, ndim = struct.unpack_from("<II", data, pos)
        shape = struct.unpack_from(f"<{ndim}I", data, pos + 8)
        pos += 8 + 4 * ndim
        dt = DTYPE_CODES[code][1]
        size = int(np.prod(shape)) * dt.itemsize
        sec[name] = np.frombuffer(data, dtype=dt, count=int(np.prod(shape)), offset=pos).reshape(shape)
        pos += size
    if pos != len(data):
        sys.exit(f"{path}: {len(data) - pos} trailing bytes")
    return dict(zip(CONFIG_KEYS, sec["config"].tolist())), sec


def check(path, hf_dir):
    """Load the int8 weights back into the float model and measure the corpus."""
    sys.path.insert(0, str(Path(__file__).parent))
    import bench
    from transformers import AutoModelForCausalLM, AutoTokenizer

    cfg, sec = read(path)
    tok = AutoTokenizer.from_pretrained(hf_dir)
    test = [m for m in bench.load_corpus() if not bench.is_train(m)]

    offs, blob = sec["tok.offsets"], sec["tok.bytes"].tobytes()
    for m in test:
        ids = tok.encode(m["text"])
        if b"".join(blob[offs[i] : offs[i + 1]] for i in ids) != m["text"].encode():
            sys.exit(f"vocab bytes do not reproduce message {m['id']}")

    def deq(name):
        return torch.from_numpy(sec[name].astype(np.float64) * unpack_scales(sec[name + ".scale"])[:, None])

    def bpc(model):
        bits = sum(bench.model_bits(model, tok, m["text"], "", 1 << 24)[1] for m in test)
        return bits / sum(len(m["text"]) for m in test)

    model = AutoModelForCausalLM.from_pretrained(hf_dir, dtype=torch.float32).eval()
    ref = bpc(model)
    with torch.no_grad():
        model.model.embed_tokens.weight.copy_(deq("embed"))
        for i, layer in enumerate(model.model.layers):
            for short, hf in LINEARS.items():
                layer.get_submodule(hf).weight.copy_(deq(f"l{i}.{short}"))
            layer.input_layernorm.weight.copy_(torch.from_numpy(sec[f"l{i}.attn_norm"] / 2**NORM_FRAC))
            layer.post_attention_layernorm.weight.copy_(torch.from_numpy(sec[f"l{i}.mlp_norm"] / 2**NORM_FRAC))
        model.model.norm.weight.copy_(torch.from_numpy(sec["final_norm"] / 2**NORM_FRAC))
    print(f"{cfg}\nvocab bytes reproduce all {len(test)} test messages")
    print(f"test split, no preamble: original weights {ref:.3f} bpc, int8 file weights {bpc(model):.3f} bpc")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("hf_dir", type=Path)
    b.add_argument("out", type=Path)
    b.add_argument("--max-ctx", type=int, default=2048)
    c = sub.add_parser("check")
    c.add_argument("model", type=Path)
    c.add_argument("hf_dir", type=Path)
    args = ap.parse_args()
    if args.cmd == "build":
        build(args.hf_dir, args.out, args.max_ctx)
    else:
        check(args.model, args.hf_dir)


if __name__ == "__main__":
    main()
