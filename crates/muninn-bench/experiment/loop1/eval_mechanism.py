#!/usr/bin/env python3
"""Loop 1, step 1 — mechanism evaluation with no model (PREREGISTRATION.md, 2026-09-17).

For each style (terse, chatty, Spanish) one store receives, as separate Claude Code sessions in time
order, every scenario's (a) original decision, then every (b) change, then every (c) unrelated later
decision — the held-out phrasings in heldout_phrasings.json, written by claude-haiku-4-5 after the
freeze. The frozen muninn binary ingests each synthetic transcript. Measured per scenario:
  retired_a   the statement (a) is no longer served (its decision record and its episode retired)
  kept_b      the change (b) is still active (not retired by (c) or by another scenario)
  served_ok   `muninn recall "<topic>"` returns the new value and not the old one
              (withdrawn scenarios: does not return the old value)
Usage: eval_mechanism.py --muninn <binary> [--out results.json]
"""
import argparse, json, os, subprocess, tempfile, uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent


def transcript(path: Path, sid: str, cwd: str, ts: datetime, user: str, assistant: str) -> None:
    u1, u2 = str(uuid.uuid4()), str(uuid.uuid4())
    base = {"isSidechain": False, "userType": "external", "cwd": cwd, "sessionId": sid}
    rows = [
        {**base, "parentUuid": None, "type": "user", "uuid": u1, "timestamp": ts.isoformat().replace("+00:00", "Z"),
         "message": {"role": "user", "content": user}},
        {**base, "parentUuid": u1, "type": "assistant", "uuid": u2,
         "timestamp": (ts + timedelta(seconds=2)).isoformat().replace("+00:00", "Z"),
         "message": {"role": "assistant", "type": "message", "model": "synthetic",
                     "content": [{"type": "text", "text": assistant}], "stop_reason": "end_turn"}},
    ]
    path.write_text("".join(json.dumps(r) + "\n" for r in rows))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--muninn", required=True)
    ap.add_argument("--out", default=str(HERE / "mechanism_results.json"))
    a = ap.parse_args()
    items = {i["key"]: i for i in json.load(open(HERE / "heldout_phrasings.json"))}
    sc = json.load(open(HERE / "scenarios.json"))
    scenarios = sc["gate3"] + sc["new"]
    results = []
    for style in ["terse", "chatty", "Spanish"]:
        root = Path(tempfile.mkdtemp(prefix=f"loop1-{style}-"))
        (root / ".git").mkdir()
        env = {**os.environ, "MUNINN_ROOT": str(root), "MUNINN_NO_PROJECT": "1"}
        subprocess.run([a.muninn, "--cwd", str(root), "init", "--keep-native"], env=env, check=True, capture_output=True)
        tdir = root / "transcripts"
        tdir.mkdir()
        t0 = datetime(2026, 9, 1, 9, 0, tzinfo=timezone.utc)
        order = [(s, "a") for s in scenarios] + [(s, "b") for s in scenarios] + [(s, "c") for s in scenarios]
        for k, (s, part) in enumerate(order):
            it = items.get(f"{s['id']}#{style}")
            if not it:
                continue
            sid = str(uuid.uuid4())
            p = tdir / f"{k:03d}-{sid}.jsonl"
            transcript(p, sid, "/work/project", t0 + timedelta(minutes=30 * k), it[part], "Noted.")
            subprocess.run([a.muninn, "--cwd", str(root), "ingest", str(p)], env=env, check=True, capture_output=True)
        subprocess.run([a.muninn, "--cwd", str(root), "--json", "export", "--all", "--out", str(root / "x.jsonl")],
                       env=env, check=True, capture_output=True)
        recs = [json.loads(l) for l in open(root / "x.jsonl")]
        for s in scenarios:
            it = items.get(f"{s['id']}#{style}")
            if not it:
                results.append({"style": style, "id": s["id"], "missing": True})
                continue
            def active_with(text):
                return [r for r in recs if not r.get("invalid") and text.strip()[:60].lower() in (r.get("body") or "").lower()]
            a_active = active_with(it["a"])
            b_active = active_with(it["b"])
            rec = subprocess.run([a.muninn, "--cwd", str(root), "recall", s["topic"]], env=env, capture_output=True, text=True).stdout.lower()
            old, new = s["old"].lower(), (s["new"] or "").lower()
            served_ok = (old not in rec) and (not new or new.split()[0] in rec)
            results.append({"style": style, "id": s["id"], "retired_a": not a_active, "kept_b": bool(b_active),
                            "served_ok": served_ok, "old_served": old in rec,
                            "a_kinds": sorted({r["kind"] for r in a_active}), "b_kinds": sorted({r["kind"] for r in b_active})})
    json.dump(results, open(a.out, "w"), indent=1)
    rows = [r for r in results if not r.get("missing")]
    def share(key, sub):
        v = [r[key] for r in sub]
        return f"{sum(v)}/{len(v)}"
    for grp, sub in [("all", rows), ("gate3 topics", [r for r in rows if r["id"].startswith("g3-")]),
                     ("new topics", [r for r in rows if r["id"].startswith("new-")])] + \
                    [(st, [r for r in rows if r["style"] == st]) for st in ["terse", "chatty", "Spanish"]]:
        print(f"{grp:14s} retired_a {share('retired_a', sub):7s} kept_b {share('kept_b', sub):7s} served_ok {share('served_ok', sub):7s} old_served {share('old_served', sub)}")
    print("missing:", sum(1 for r in results if r.get("missing")))


if __name__ == "__main__":
    main()
