#!/usr/bin/env python3
"""For each scenario of a fixture, does the store hold the new value and not the old one?

A grid's cell is an agent reading a delivery and writing a file, and it costs a model call.
What the engine controls is narrower and can be read directly: after capture, is the new value
in an active record and the old one out of one? This answers that offline, from the committed
seeding snapshots, with no model in the loop.

It is a proxy, not the cell: an agent can still read a correct store and write the wrong thing,
and it can read an incomplete one and get it right from the episodes. What it is good for is
telling a capture change that helps from one that quietly retires the answer — which counting
typed and retired records cannot, because both move in both directions for both reasons.

  store_answers.py [--binary target/release/muninn] [--grids h2h-v32-v3 ...]
"""
import argparse, json, os, re, shutil, sqlite3, subprocess, tempfile
from pathlib import Path

from store_shape import as_transcript, DEFAULT_GRIDS

HERE = Path(__file__).resolve().parent
EXP = HERE.parent


def word_in(text: str, value: str) -> bool:
    """`value` as a whole word of `text`, case-folded. A scenario's value can be several words
    (`LRU with a 300-second TTL`), and what identifies it is its first token."""
    head = re.split(r"\s+", value.strip())[0].lower()
    return re.search(rf"(?<![\w.-]){re.escape(head)}(?![\w.-])", text.lower()) is not None


def answers(binary: Path, seed_file: Path, scenarios: list) -> dict:
    root = Path(tempfile.mkdtemp(prefix="answers-"))
    try:
        env = {**os.environ, "MUNINN_ROOT": str(root / ".muninn"), "MUNINN_NO_PROJECT": "1"}
        subprocess.run(["git", "init", "-q", str(root)], capture_output=True)
        run = lambda *a: subprocess.run([str(binary), "--cwd", str(root), *a], env=env,
                                        capture_output=True, text=True)
        run("init", "--keep-native")
        t = root / "seed.jsonl"
        as_transcript(seed_file, t)
        run("ingest", str(t))
        run("maintain")
        db = sqlite3.connect(root / ".muninn" / ".muninn" / "muninn.db")
        typed = [r[0] for r in db.execute(
            "SELECT object FROM served_record WHERE kind IN "
            "('decision','invariant','deadend','correction')")]
        every = [r[0] for r in db.execute("SELECT object || ' ' || body FROM served_record")]
        db.close()
        out = {}
        for sc in scenarios:
            old, new = sc["old"], sc["new"]
            out[sc["id"]] = {
                "new_typed": any(word_in(o, new) for o in typed),
                "old_typed": any(word_in(o, old) for o in typed),
                "new_served": any(word_in(o, new) for o in every),
                "old_served": any(word_in(o, old) for o in every),
            }
        return out
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=str(EXP.parents[2] / "target/release/muninn"))
    ap.add_argument("--grids", nargs="*", default=DEFAULT_GRIDS)
    ap.add_argument("--json")
    a = ap.parse_args()
    rows = []
    for g in a.grids:
        d = EXP / "results" / g
        if not (d / "seeding").exists():
            print(f"{g}: no seeding snapshot")
            continue
        cfg = json.load((d / "config.json").open())
        scenarios = [{"id": t["id"], **t["scenario"]} for t in cfg["tasks"]
                     if t["scenario"].get("new") is not None]
        for f in sorted((d / "seeding").glob("r*.jsonl")):
            for sid, v in answers(Path(a.binary), f, scenarios).items():
                rows.append({"grid": g, "seed": f.name, "scenario": sid, **v})
    if a.json:
        Path(a.json).write_text(json.dumps(rows, indent=1))
    grids = sorted({r["grid"] for r in rows})
    print(f"{'grid':16} {'clean':>8} {'new typed':>10} {'old still typed':>16} {'cells':>6}")
    for g in grids:
        sub = [r for r in rows if r["grid"] == g]
        clean = sum(1 for r in sub if r["new_typed"] and not r["old_typed"])
        newt = sum(1 for r in sub if r["new_typed"])
        oldt = sum(1 for r in sub if r["old_typed"])
        print(f"{g:16} {clean:>8} {newt:>10} {oldt:>16} {len(sub):>6}")


if __name__ == "__main__":
    main()
