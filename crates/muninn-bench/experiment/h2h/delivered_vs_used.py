#!/usr/bin/env python3
"""Was the memory wrong, or was the answer? (PREREGISTRATION.md, same title.)

A head-to-head cell fails two ways that are not the same thing: the memory did not put the
current decision in front of the agent, or it did and the agent wrote something else. The
first is a memory result; the second is a statement about how an agent treats injected
context, and counting it against the memory makes a grid that improving the memory cannot
move.

For every cell, this reads the cell's own transcript for what the arm injected — the
`hook_additional_context` and `hook_system_message` attachments, which is everything a hook
put in front of the model — and asks whether the scenario's **new** value is in it, before
the agent's first answer. Crossed with the oracle:

    used                        delivered and the oracle passed
    delivered and not used      delivered and the oracle failed
    passed without memory       not delivered and the oracle passed anyway
    not delivered               not delivered and the oracle failed

It is a floor on delivery: a substring search cannot tell a block that states the value from
one that merely contains the word, and it says nothing about whether the block was legible.

  delivered_vs_used.py <results dir> [<results dir> ...]
"""
import argparse
import json
import pathlib

INJECTED = ("hook_additional_context", "hook_system_message")


def injected_text(path: pathlib.Path) -> str:
    if not path.exists():
        return ""
    out = []
    for line in path.open(errors="replace"):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if r.get("type") != "attachment":
            continue
        at = r.get("attachment") or {}
        if at.get("type") not in INJECTED:
            continue
        c = at.get("content")
        out.append(" ".join(c) if isinstance(c, list) else str(c))
    return " ".join(out).lower()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    a = ap.parse_args()
    print("| grid | arm | used | delivered, not used | passed without memory | not delivered |")
    print("|---|---|---|---|---|---|")
    for d in a.dirs:
        d = pathlib.Path(d)
        if not (d / "results.jsonl").exists():
            continue
        cfg = json.loads((d / "config.json").read_text())
        sc = {t["id"]: t["scenario"] for t in cfg["tasks"]}
        rows = [json.loads(l) for l in (d / "results.jsonl").read_text().splitlines() if l.strip()]
        tally: dict = {}
        for r in rows:
            s = sc[r["task"]]
            if s["new"] is None or r.get("error"):
                continue
            if r["arm"] == "off":
                continue
            text = injected_text(d / "logs" / f"r{r['run']}-{r['task']}-{r['arm']}.transcript.jsonl")
            new = s["new"].lower().split(" with ")[0]
            delivered = new in text
            passed = r["status"] == "pass"
            key = (delivered, passed)
            t = tally.setdefault(r["arm"], {k: 0 for k in
                                            [(True, True), (True, False), (False, True), (False, False)]})
            t[key] += 1
        for arm, t in sorted(tally.items()):
            print(f"| {d.name} | {arm} | {t[(True, True)]} | {t[(True, False)]} | "
                  f"{t[(False, True)]} | {t[(False, False)]} |")


if __name__ == "__main__":
    main()
