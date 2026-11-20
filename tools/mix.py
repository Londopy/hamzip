"""Write many synthetic messages, spliced from corpus lines, for checking that two machines encode alike.

Run `hamzip bench <out>` on each machine; the digests must match.
"""

import argparse
import json
import random
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("out", type=Path)
    ap.add_argument("--count", type=int, default=2000)
    ap.add_argument("--seed", type=int, default=73)
    args = ap.parse_args()

    lines = []
    for path in sorted((ROOT / "corpus").glob("*.jsonl")):
        for row in path.read_text(encoding="utf-8").splitlines():
            if row.strip():
                lines.extend(x for x in json.loads(row)["text"].split("\n") if x.strip())
    rng = random.Random(args.seed)
    args.out.mkdir(parents=True, exist_ok=True)
    # a leftover larger set would change the digest
    for old in args.out.glob("*.txt"):
        old.unlink()
    for i in range(args.count):
        text = "\n".join(rng.choice(lines) for _ in range(rng.randint(1, 8)))
        (args.out / f"{i:05d}.txt").write_bytes(text.encode())
    print(f"wrote {args.count} messages to {args.out}")


if __name__ == "__main__":
    main()
