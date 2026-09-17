<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/muninn-dark.png">
    <img src="assets/muninn-light.png" alt="Muninn" width="480">
  </picture>
</p>

# Muninn

Muninn gives AI coding assistants (Claude Code and Codex) a memory that drops what you replaced.

Most memory tools search by resemblance. You ask a question, they return the notes that look most
like it. A decision you changed your mind about looks just as relevant as the one that replaced
it, so the assistant gets both and picks one.

Muninn stores decisions rather than notes: a topic, a value, who said it, and where in the
conversation. Once you decide something else about the same topic, the earlier decision leaves the
set the assistant can see. It stays in your history, and you can ask for it whenever you want.

Everything runs on your computer. There is no server, no account and no cloud service. The part
that answers the assistant on every turn uses no AI model at all. There is also an optional local
embedding model you install yourself, which helps `muninn why` find things and tidies the store
after a session; without it everything still works by keyword.

## What it does

Muninn remembers what you decided, for as long as it holds. Decisions, corrections you made, rules
you stated, approaches that failed. When a later question touches one of them, the assistant gets
your words as you wrote them, plus a pointer into the transcript when Muninn captured one.

It also turns written rules into enforced ones. A line in `CLAUDE.md` or `AGENTS.md` such as
"never edit the migrations folder" can become a permission setting the assistant's tool obeys,
instead of a sentence it may or may not follow. Nothing changes until you have seen the proposed
change and said yes.

## How it works

When a session ends, Muninn reads the conversation and keeps the parts that are decisions,
corrections, rules or dead ends. Each one becomes a small card: the topic, the value, where it
came from, and the place in the transcript it came from. The card holds your words verbatim, so
nothing is quietly reworded into something you did not say.

When a later message replaces one of those decisions, Muninn marks the earlier card as superseded.
Noticing the replacement is the hard part, and Muninn gets it wrong a fair amount of the time; the
score is in "Does it work?" below. What happens after the mark is the part that is reliable. The
card is not deleted, it keeps a label saying what replaced it, and it stops being something the
assistant can be shown.

On each turn, Muninn looks at what you are writing and which files you are touching, finds the
cards that relate to it, and passes them to the assistant as evidence rather than as instructions,
capped at 700 tokens. That takes a few thousandths of a second and calls no model, so the same
store and the same question give you the same answer every time.

The second step is where Muninn differs from a memory that searches by resemblance. There a
retired note is still a note, and the most you can do is push it down the ranking. Here the
queries that feed the assistant read from a filtered view of the database, and the type that
carries a card into the assistant's context has a single constructor, which reads that view. The
view also leaves out the columns a stale hand-written filter would name, so such a filter fails
outright, and a test in CI rejects any new query that goes around the view.

## An example

Monday, you tell the assistant:

> for the transport compression codec we go with gzip

Thursday, you change your mind:

> change of plan — the transport compression codec is now zstd

Two weeks later, in a new session, you ask the assistant to write up the project's compression
policy. It remembers neither conversation, so it asks its memory tool.

A memory that searches by resemblance finds both notes, scores them about the same, and leaves the
assistant to guess which one is current. Muninn passes it one card:

```
[muninn:decision] #2 · a1b2c3d4 · origin: user_said · trust 3
user: change of plan — the transport compression codec is now zstd.
  evidence: ~/.claude/projects/your-project/a1b2c3d4-….jsonl:48213
```

The block tells the assistant what kind of card this is, its number, that you said it yourself
(`origin: user_said` with `trust 3`, the top of four levels), and where to go and check
(`evidence:` gives the transcript file and the position in it). Cards Muninn is less sure about
arrive labelled as hints rather than facts. The full vocabulary is in
[`docs/format.md`](docs/format.md).

The gzip decision still exists. Ask for it and it comes back with its label:

```sh
muninn why --all "compression codec"
```

```
[muninn:decision] #1 · policy.compression is · origin: user_said · trust 3 · 2026-08-31T09:41:00Z · RETIRED (superseded)
user: for the transport compression codec we go with gzip.
```

Without `--all` you get only the decision that still stands.

## Why not just a normal memory tool?

The middle column is how retrieval by similarity works by design, not a measurement of any
particular product.

| when this happens | a memory that searches by resemblance | Muninn |
|---|---|---|
| you changed your mind about something | returns the old note and the new one, ranked by how well each matches your words | the replaced one is not in the list |
| you ask what you decided *not* to do | has no way to say "we ruled this out"; you get whatever mentions the topic | dead ends are one of the card kinds, and are searched as one |
| a rule was revoked | the revoked rule is still text in the store, and still retrievable | revoked cards cannot reach the assistant; you see them when you ask |
| what a turn costs you | a retrieval step whose size you do not control | a few thousandths of a second, no model call, capped at 700 tokens |

Two findings from the published literature. These are other people's measurements, not ours, and
the evidence ids point into [`research/00-evidence-log.md`](research/00-evidence-log.md):

- On questions of the form "is this still true?", "is this the complete set?" and "what did we
  decide against?", a production vector memory tool answered 6–27 % correctly, against 98–100 %
  for typed records with explicit supersession links, at the same relevance and the same token
  cost `[K6]`.
- Of five memory systems loaded with a revoked policy and its replacement, none enforced the
  revocation by default: the revoked fact came back, outranked its own replacement, and led the
  agent to the unsafe action `[K7]`. That result covers those five systems. It is not a claim
  about every memory tool in existence. Three other systems in the same evidence log do handle
  revoked or conflicting evidence deterministically (`[U5]`, `[S3]`, `[S5]`), and Muninn's own
  novelty claim was rewritten once that came to light.

## Does it work?

Every result below comes from a test whose rules were written and timestamped before it ran, and
the raw data is in this repository. Most of the grids can be re-run from
[`REPRODUCE.md`](crates/muninn-bench/experiment/REPRODUCE.md); the ones seeded from private
transcripts of this project cannot, and they say so.

The first claim is not a score. A retired decision is never handed to the assistant, because the
queries that feed it read from a filtered view of the database and the type that carries a card
into its context cannot be built from anything else. A test drives the real hooks 200 times per
run, including with the database corrupted, the schema version set to one the binary refuses, and
the clock moved backwards, and fails if a retired value appears in the output. It was checked the other way round
as well: undo the filter and the test goes red naming the value that leaked.

The rest are measurements:

| test | with Muninn | without memory |
|---|---|---|
| The assistant needs a fact that only an earlier session contains (tasks on this project) | 19 of 25 tasks solved | 2 of 25 |
| The assistant is given notes in which the replaced decisions are already marked, on three open-source projects | 161 of 162 tasks correct | 3 of 162 |
| Important rules survive when the conversation is compressed to save space | 100 of 100 compressions | not measured here |

Some results were weaker, and they are published too. The same fact test on Codex gave 10 of 25
against 5 of 25, which is not a clear difference. Two features did not help and were removed or
turned off by default.

The comparison with other memory tools (claude-mem and agentmemory) gives every tool the same
decisions from the same real sessions, and nobody tells it which decisions were replaced.
Version 0.1.0 lost it to claude-mem (12 of 27 against 26 of 27). Version 0.2.0 was then tested on
new wording that no version had seen: it got 17 of 27, claude-mem 14 of 27 and agentmemory 9 of 27.
That difference is too small to call Muninn better, so it counts as a tie with both. Muninn's
answers also mentioned the replaced value more often (11 of 27 against 2 of 27 for claude-mem).

## Where it is weak

Noticing that you changed your mind is the weak half of the job. Everything above about retired
decisions assumes the decision got marked as retired in the first place, and that marking is a set
of rules over the words you typed. On wording no version of Muninn had seen, it caught 17 of 27
replacements against claude-mem's 14, which is a tie. The guarantee protects what happens after
the mark; getting the mark right is ordinary work, and ours is ordinary.

The fact-recall numbers come from tasks we wrote, about this repository. 19 of 25 against 2 of 25
follows a rule written before the run, and it is still a measurement of our own tasks on our own
code. The Gate 3 grid was re-run on three outside projects. Gate 2 could not be, because its facts
live in this project's transcripts and do not move.

Retired records stay on disk in plain sight. Muninn mirrors every record to `.muninn/records/` as
Markdown, retired ones included and labelled as retired. Nothing hands them to the assistant, but
an assistant that greps the folder will find them. `MUNINN_NO_PROJECT` turns the mirror off.

The speed figures were measured on one machine. CI enforces the same limits on every run, so a
regression fails the build, but the figures are not a promise about your hardware.

There is no head-to-head against Mem0, Zep, Letta or the assistant's own built-in memory. Those
have not been run on the same harness, so there is no comparison to report and none is implied.

The full list of what is claimed, what is not, and the limits of each result is in
[`docs/claims.md`](docs/claims.md). What Muninn deliberately does not do is in
[`docs/scope.md`](docs/scope.md). The commands to reproduce every number are in
[`crates/muninn-bench/experiment/REPRODUCE.md`](crates/muninn-bench/experiment/REPRODUCE.md).

## Install

You need [Rust](https://rustup.rs) and Claude Code or Codex.

```sh
git clone https://github.com/ilien-dev/muninn
cd muninn
cargo build --release -p muninn-cli
mkdir -p plugin/bin && cp target/release/muninn plugin/bin/
claude plugin add ./plugin
```

Then, inside your own project:

```sh
muninn init          # set up memory for this project
muninn init --codex  # also set it up for Codex
muninn status        # check that everything is working
```

`muninn clean --yes` removes everything `init` added.

## Everyday use

Once it is set up, you don't need to do anything. Muninn works in the background while you use
the assistant. A few commands are useful when you want to look inside:

```sh
muninn why "why did we choose exponential backoff"   # what stands now, and the id of what it replaced
muninn why --all "exponential backoff"               # the same, retired records included
muninn revoke 142 --reason "no longer true"          # retire a record by hand (it stays in the history)
muninn export --all                                  # every record, as a file you can read
muninn compile && muninn apply                       # turn written rules into enforced ones
```

## For developers

The code is a Rust workspace. The storage layer is SQLite with full-text search.

```
crates/muninn-core      storage, search, filtering
crates/muninn-cli       the Muninn program and its hooks
crates/muninn-capture   reads conversations and git history
crates/muninn-compile   turns written rules into permission settings
crates/muninn-embed     optional local embedding model: used when writing and by `muninn why`, never in a hook
crates/muninn-symbols   map of the code's functions and types
crates/muninn-why       answers `muninn why`
crates/muninn-bench     speed tests and the experiments behind every number
plugin/                 Claude Code plugin
codex/                  Codex setup
```

Tests:

```sh
cargo test --workspace --features exact-tokens
MUNINN_FAULT_REPS=200 cargo test -p muninn-cli --release --test fault
cargo run --release -p muninn-bench -- perf --strict
```

The design and the reasons behind it are in [`design/ENGINE.md`](design/ENGINE.md) and
[`research/CONCLUSION.md`](research/CONCLUSION.md). Changes between versions are in
[`CHANGELOG.md`](CHANGELOG.md).

## About the name

<img src="assets/muninn-symbol.png" alt="" width="96" align="right">

In Norse myth, Odin keeps two ravens. They fly out over the world at dawn and come back at supper
to tell him what they saw. Huginn is thought; Muninn is memory. In the *Grímnismál*, one of the
poems of the Poetic Edda, Odin says he fears Huginn may not come back, and that he fears more for
Muninn.

That always struck me as the right way round. Thought can be done again. A memory that does not
come back is simply gone.

A coding assistant is Huginn. It thinks fast, it ranges wide, and every morning it starts from
nothing. Muninn is the one that comes back carrying what you decided.

The mark is a raven inside a broken ring. The ring is a decision that no longer holds: kept,
visible, and no longer flown.

## License

Muninn is © 2026 ilien and licensed under the
[GNU Affero General Public License, version 3 only](LICENSE), with the additional
terms in [`NOTICE`](NOTICE):

- Anyone may use, modify, host and sell Muninn, including as a cloud service.
- Anyone who distributes a modified version, or offers one to users over a network, must publish its
  complete source code under the same license.
- Every copy and every derived version must keep the notice
  "Based on Muninn by ilien - https://github.com/ilien-dev/muninn", and a modified version must be
  marked as changed.

Contributions need a one-time [Contributor License Agreement](CLA.md); see
[`CONTRIBUTING.md`](CONTRIBUTING.md).
