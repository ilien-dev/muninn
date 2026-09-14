# Gate 2 experiment — 96 cells, model claude-sonnet-5, 5 run(s)

| arm | non-inferable pass | inferable pass | errors | delivered tokens (mean) | hook p95 ms (max) | cost/cell (mean) |
|---|---|---|---|---|---|---|
| off | 0/24 (0%) | n/a | 0 | 0 | 0.02 | $0.330 |
| literal | 14/24 (58%) | n/a | 0 | 1188 | 1.89 | $0.280 |
| control | 2/24 (8%) | n/a | 0 | 1041 | 2.19 | $0.288 |

literal − off (non-inferable): +0.583 [95% CI +0.458, +0.708]
control − off (non-inferable): +0.083 [95% CI +0.000, +0.167]

Gate 2: PASS

## Per task

| task | inferable | off | literal | control | 
|---|---|---|---|---|
| engine-s12-why-responder | false | 0/0 | 0/0 | 0/0 | 
| fact-userprompt-p95 | false | 0/0 | 0/0 | 0/0 | 
| fact-sessionstart-gate | false | 0/0 | 0/0 | 0/0 | 
| fact-corpus-fetch-limits | false | 0/0 | 0/0 | 0/0 | 
| fact-real-transcript-ingest | false | 0/0 | 0/0 | 0/0 | 
| docs-grammars-control | true | 0/0 | 0/0 | 0/0 | 

Retired records delivered (all arms, all cells): 0.

Total model cost: $28.03. Errors are excluded from pass rates and listed in results.jsonl.
