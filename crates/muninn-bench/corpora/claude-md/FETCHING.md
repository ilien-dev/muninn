# Corpus fetch: rate limit and hosts

## Host used to fetch files

- `raw.githubusercontent.com` — the only fetch host stated in the repo, per the
  `manifest.tsv` header: "public CLAUDE.md / AGENTS.md files located via GitHub
  code search (grep.app) ... fetched raw via raw.githubusercontent.com".
  `grep.app` was used to *locate* candidate files (code search), not to fetch
  their content.

No other CDN or mirror host (e.g. jsdelivr, statically) appears anywhere in
this repo's fetch-related files (`manifest.tsv`, `GATE1.md`, `unavailable.tsv`,
or `muninn-bench` source). If any were used, it isn't recorded here.

## Rate limit

`GATE1.md` states only that "fetching is rate-limited" and that the corpus was
"fetched incrementally" as a result (115 of 323 files on disk at the gate
measurement, 322 of 323 by the next day). No file in this repo gives the
per-domain visits-per-window count, the window length, or the cooldown after a
breach. Not inventing those numbers here — if the real limit config exists, it
lives outside this repo (e.g. in fetch-tool state, not checked into it).
