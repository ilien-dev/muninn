# History rewrite of 2026-09-14

Before the repository is published, the maintainer's e-mail address was removed from two result
files (`results/pmbench/round8-sonnet/launcher.log` and `canary2.json`: a bridge canary had
quoted the harness's account context). `git filter-repo --replace-text` replaced the address with
`[account e-mail]` in every commit; nothing else changed. Checked on a clone before the refs moved:
the same 105 commits with identical messages, an identical tree at HEAD, and 0 stored objects
containing the address afterwards.

Commit hashes from `19e9e70` onwards changed (27 commits). The full old → new map is
`history-rewrite-map.json`; run manifests and `FROZEN.json` files recorded the old hash that was
HEAD when they ran, and are left as recorded.

## Effect on the timestamp proofs

| proof | commit it names | anchored | after the rewrite |
|---|---|---|---|
| `round8-f173f2d.txt.ots` | `f173f2d` | Bitcoin block 966829 | unchanged |
| `gate3-codex-public-rep5-6313554.txt.ots` | `6313554` | Bitcoin block 966829 | unchanged |
| `instrument-fix-6e1cadb.txt.ots` | `6e1cadb` | Bitcoin block 966832 | unchanged |
| `restart-cd52e86.txt.ots` | `cd52e86` | Bitcoin block 966832 | unchanged |
| `round9-19e9e70.txt.ots` | `19e9e70` | Bitcoin block 966895 | now `d375a26`; re-stamped in `rewrite-2026-09-14.txt.ots` |
| `external-c357381.txt.ots` | `c357381` | Bitcoin block 966933 | now `8d8ca81`; re-stamped in `rewrite-2026-09-14.txt.ots` |
| `gate2-codex-fix-3be3593.txt.ots` | `3be3593` | calendar, pending confirmation | now `742f751`; re-stamped in `rewrite-2026-09-14.txt.ots` |

The four proofs on unchanged commits — including the one that fixes the held-out PM-Bench seeds
(`f173f2d`) — verify against this repository as published. The three on rewritten commits still
prove that the *old* hashes existed at the anchored time; the old objects are kept, private, in a
backup bundle (sha256 below) that can be shown to a reviewer, and the new hashes were stamped again
after the rewrite. Those three pre-registrations (round 9, the external repositories, the Gate 2
Codex fix) are therefore publicly timestamped only from the rewrite date; their order relative to
the results remains visible in the commit sequence, which is not a proof.

Backup bundle (private, not in the repository): sha256 `7f5d374db89e96e94e318a3aba50699e557e7291cabb7f86bce176c118bbb31c`.
