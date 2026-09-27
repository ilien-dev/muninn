# Why Muninn

This page collects, in one place, where Muninn is measurably ahead of the other memory tools for
coding assistants, and where it is not. Each figure below comes from a test with its rules fixed
before it ran. The raw data is in this repository, and each row names its grid. This page draws on [`claims.md`](claims.md). Where the two disagree, `claims.md` wins. Names such
as v17 or v43 are test runs, numbered in the order they were registered, not versions of Muninn;
[how to read the test names](technical-overview.md#how-to-read-the-test-names) explains them.

## The short version

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/motion/results-dark.gif">
    <img src="../assets/motion/results-light.gif" width="760"
         alt="Bar chart. Changed in conversation: Muninn 252 of 270, claude-mem 208 of 268. Change also in the code: Muninn 54 of 54, claude-mem 25 of 54, agentmemory 1 of 54. Old value written back: Muninn 0 of 54, claude-mem 10 of 54.">
  </picture>
</p>

- **When you change your mind, the assistant follows the new decision.** Across five different
  ways of phrasing a change, Muninn got the assistant to the current answer in **252 of 270**
  tasks, and claude-mem in **208 of 268** (p = 1.7 × 10⁻⁷). When the change also reached the
  code, the scores were **54 of 54** for Muninn, 25 of 54 for claude-mem and 1 of 54 for
  agentmemory.
- **A decision you replaced never comes back.** Every path that feeds the assistant reads a view with no retired rows in it, and CI tests that on every commit.
- **It is fast and it stays on your machine.** A hook costs about 3 ms at 20 000 records. There
  is no server, no account and no cloud service, and no AI model runs in the path that answers
  the assistant.
- **The same conversation gives the same memory.** Three runs of the same twenty sessions stored
  the same records. claude-mem's stored text did not overlap at all between two runs.
- **Written rules can become enforced ones.** A line such as "never push to main" can become a
  control the assistant's tool obeys. When the model ignored the rule, the control still stopped
  the action.

## Head to head

Every competitor row below comes from the same harness and the same tasks: the same live
sessions seeded every tool's memory, each tool captured them through its own shipped hooks, and
a deterministic check graded what the assistant wrote. No judge model graded anything.

### A decision replaced in conversation only (test run v43)

Both products ran in one grid, and each built its own memory from the same sessions. Six runs,
54 tasks per phrasing for each tool.

| how the change was phrased | Muninn | claude-mem 13.24.23 |
|---|---|---|
| a bare value (`zstd`) | **40/54** | 28/53 |
| a sentence | **54/54** | 32/53 |
| a comparison | 54/54 | 50/54 |
| a sentence with a change verb | 52/54 | 47/54 |
| mixed | 52/54 | 51/54 |
| **all five** | **252/270** | **208/268** |
| replaced value written anyway | **2** | 6 |

Muninn wins two phrasings outright and ties the other three. Source:
`crates/muninn-bench/experiment/results/h2h-v43-*`.

### A decision replaced in conversation and in the code (test run v17)

| | Muninn | claude-mem 13.24.23 | agentmemory 0.9.29 | no memory |
|---|---|---|---|---|
| current answer | **54/54** | 25/54 | 1/54 | 0/54 |
| replaced value written anyway | **0/54** | 10/54 | — | 3/54 |

Against claude-mem, p = 2.0 × 10⁻¹¹. Against agentmemory, p = 4.4 × 10⁻³⁰. On the shipped build,
agentmemory also read 1/49 against Muninn's 51/54. Source: `results/h2h-v17/`.

### What each task cost (exploratory)

Nobody registered this reading before the run, so it is **not a claim**. It comes from the same
v43 tasks (300 for Muninn, 298 for claude-mem). Source: `results/h2h-v43-cost.json`, written by
`h2h/v43_cost.py`.

| per task | Muninn | claude-mem |
|---|---|---|
| cost reported by the assistant | **$0.059** | $0.098 |
| turns the assistant took | **3.9** | 8.1 |
| time | **11.5 s** | 24.7 s |

Muninn puts more memory into the context: about 2.4 times claude-mem's share of the window. The
assistant then goes straight to the answer instead of searching for it, and on these tasks the
whole task came out cheaper. claude-mem also calls a model in the background to summarise
sessions. Those calls are not in this table. The tasks are short, one message each; in a long
session the per-message block adds up.

## What makes it different

Each record Muninn keeps is a topic, a value, who said it, and where it was said, in the words
used at the time. No model summarises or rewrites them. Published work found that memory
rewritten this way does worse than no memory at all `[C1]` `[K10]`.

In a memory that works by resemblance, a replaced note is still a note, and the best you can do
is push it down the list. In Muninn every path that feeds the assistant reads from a view that
has no retired rows, and a CI test rejects any new query that goes around that view.

Muninn also reads the repository. When a commit removes a value from the code, Muninn retires
the decision that named it, even if the new value never shares a word with the old one. Two
held-out sets retired 29 of 30 decisions this way, with 0 false retirements in every condition
(loops 8 and 9).

No model runs on the read path, so the same store and the same question give the same answer
every time. The hooks are held to hard latency limits in CI: SessionStart at 2.8 ms and a full
prompt at 4.0 ms (p95), at the schema's cap of 20 000 records.

A write-path model went through end-to-end tests before this page existed. A 9B model on the CPU
found the replaced record, but it also marked related ones, and it added nothing on the grid: 46
of 54 with it against 48 of 54 without, in Spanish with every change separated from its decision
`[Z11]`. What already works there is the delivery: what was said, listed newest first, as the
latest word on a subject. On that same Spanish fixture, where the rules retired only four to
seven records per store, Muninn still read 48 of 54 without any model (v42). There is no
competitor figure on that fixture.

Rules can be enforced as well as read. On 53 hand-labelled tool calls, the control compiled from
a written rule blocked 23 of 25 forbidden calls and 0 of 28 allowed ones (Gate 5a). With the
rule hidden from the model, violations fell from 8 of 24 to 0 of 24 (Gate 5b).

Standing rules survive compaction: in 100 forced compactions, Muninn delivered all 10 of them
each time (Gate 4).

Claude Code's automatic memory does not run in scripted `-p` sessions. Muninn's hooks do. This
is a statement about that session type, not a comparison of the two memories.

## What has not been measured, and why

| alternative | status |
|---|---|
| claude-mem | measured, above |
| agentmemory | measured, above |
| Mem0 | **not run.** Mem0 extracts memories with an LLM. On the local model its own example names, its prompt extracted 0 of 4 engineering decisions, so the arm would score 0 and measure that model, not Mem0. Running it with its production model needs an OpenAI key, and this test did not use one. |
| Claude Code's built-in memory | **not comparable** on this harness: it does not operate in the non-interactive sessions every task uses |
| Zep, Letta, Rekal and others | **not run** on the same harness; no comparison with them exists |

Figures that vendors publish on LoCoMo or LongMemEval measure other things, under other
harnesses. This page does not compare them.

## Where it is behind

- **Context.** Muninn's memory blocks take about 2.4 times the room claude-mem's do. Delivery on
  every prompt can be switched off (`muninn config prompt-delivery off`), which costs about four
  correct answers in fifty.
- **Scope of the win.** The head-to-head covers one task family: acting on the current decision
  after it changed. Recall of loose facts, long histories and searching old material have not
  been compared.
- **Bare values still cost.** A turn that is only a value, with a reply that confirms nothing,
  gives the rules nothing to record. Muninn now wins that phrasing (40/54 against 28/53), but it
  is still its weakest row.
