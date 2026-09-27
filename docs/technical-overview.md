# How Muninn works, and how well

This is the technical companion to the [README](../README.md): how Muninn stores and retires
decisions, every measurement behind the numbers the README quotes, where it is weak, and how to
build it. [`why-muninn.md`](why-muninn.md) is the short comparison with other tools, and
[`claims.md`](claims.md) is the full ledger of what is and is not claimed.

## How to read the test names

The results below come from tests that each had their rules written, and timestamped, before
they ran. They are named the way the project log names them. None of these names is a version of Muninn;
releases are numbered like 0.2.0.

- **v17, v41, v43, …** are head-to-head runs, numbered in the order they were registered. In
  each one the same live sessions fill every memory tool, and the assistant is then asked to do
  tasks that depend on a decision that was later changed. v17 is the run where the change also
  reached the code; v43 is the run on five different phrasings of the change, with Muninn and
  claude-mem in the same run.
- **Gate 1 to Gate 5** are the acceptance tests fixed when the project was designed: rule
  classification (1), facts recalled from earlier sessions (2), retired records kept away from
  the assistant (3), rules surviving compaction (4), and written rules enforced (5a checks the
  refusals, 5b whether the assistant behaves differently).
- **Loop 1, loop 8, …** are rounds of the retirement tests: can Muninn tell that a new message
  replaced an old decision, and does it leave unrelated decisions alone.
- **`[K6]`, `[Z11]`, …** point to entries in the evidence log,
  [`research/00-evidence-log.md`](../research/00-evidence-log.md).

Every run is registered in
[`experiment/PREREGISTRATION.md`](../crates/muninn-bench/experiment/PREREGISTRATION.md), with its
raw data under `crates/muninn-bench/experiment/results/`.

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
[`docs/format.md`](format.md).

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

Two findings from the published literature. These are other people's measurements, not ours. The
evidence ids point into [`research/00-evidence-log.md`](../research/00-evidence-log.md):

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
[`REPRODUCE.md`](../crates/muninn-bench/experiment/REPRODUCE.md); the ones seeded from private
transcripts of this project cannot, and they say so.

The first claim is not a score. A retired decision is never handed to the assistant, because the
queries that feed it read from a filtered view of the database and the type that carries a card
into its context cannot be built from anything else. A test drives the real hooks 200 times per
run, including with the database corrupted, the schema version set to one the binary refuses, and
the clock moved backwards, and fails if a retired value appears in the output. It was checked
the other way round as well: undo the filter and the test goes red naming the value that leaked.

The rest are measurements:

| test | with Muninn | without memory |
|---|---|---|
| The assistant needs a fact that only an earlier session contains (tasks on this project) | 19 of 25 tasks solved | 2 of 25 |
| The assistant is given notes in which the replaced decisions are already marked, on three open-source projects | 161 of 162 tasks correct | 3 of 162 |
| Important rules survive when the conversation is compressed to save space | 100 of 100 compressions | not measured here |
| A rule you wrote, turned into a setting, actually refuses the command it forbids | 23 of 25 refused | — |
| …and leaves alone the commands that rule still allows | 28 of 28 left alone | — |

A refusal is not yet a better outcome, so that was measured too, and the first answer was a
flat no. Given the same eight rules written in `CLAUDE.md`, turning them into enforced settings
changed nothing across 48 runs: the assistant broke none of the rules either way. It had read
them and worked around them on its own: it committed one file instead of everything and fixed a
directory's permissions instead of reaching for `sudo`. On a model that already does what your
file says, the setting has nothing left to do.

What it is for is the case where that does not happen. With the rule taken out of the
assistant's view and only the setting left in place, the forbidden action happened in 8 of 24
runs without it and 0 of 24 with it. Both numbers, and the two bugs this test found in
our own code, are in [`docs/claims.md`](claims.md).

The last two rows took three tries. The first two attempts failed, and they are in the
repository with the reasons: the first because the settings Muninn wrote were broader than
the rules they came from (a rule about `main` refused every force-push, a rule about
`pkill -f zellij` refused every `kill`), and the second because four rules it should have
caught were not caught, one of them because of a bug in Muninn itself. Both were fixed, and
the numbers above come from a third set of rules neither attempt had seen.

Noticing that a decision was replaced used to be the weak half, and the reason was measured:
of 30 held-out cases, 23 have no word in common between the old decision and the message
replacing it, so no amount of word-matching reaches them. That is still true. Muninn now looks
somewhere else as well.

When a decision reaches the code, the code says when it stops being true. Muninn reads the
diffs of your commits. A value that a commit took out and that no tracked file holds any more
is a value the project has stopped using. So is one that a single line of a commit replaced
with another. The decision that named it is retired. What the line became is
recorded too, with the commit behind it, so the question that reached the old answer reaches
the new one. Nothing is guessed: a removed word only counts if a record already named it, a
word your repository uses in more than three files is not treated as a value at all, and a
commit that retires nothing writes nothing.

Measured on two held-out sets that the code had never seen, with the commit subject
deliberately uninformative ("update dependencies") so the commit contributes only its diff:

| | from the conversation alone | with the commits | from the commits alone |
|---|---|---|---|
| the replaced decision is retired | 17 of 30 | 29 and 30 of 30 | 29 of 30 |
| …when ten other decisions were taken in between | 5-6 of 30 | 29 of 30 | 29 of 30 |
| the current answer is delivered | 8 and 11 of 30 | 19 and 21 of 30 | 21 and 29 of 30 |
| …when ten other decisions were taken in between | 0 and 1 of 30 | 17 and 25 of 30 | 21 and 29 of 30 |
| a commit that changes something unrelated retires a decision | — | 0 of 30, every condition | 0 of 30 |

In the third column the user states a decision **once and never mentions it again**, and the
commit subject says only "update dependencies". Everything the memory knows about the change, it
read from the diff.

The second row is where the commits matter most. Every word-matching rule needs the two messages
to be near each other, because that is the only thing relating them when they share no words; a
commit relates them by value, and does not care how long ago you said it.

This has limits. A decision that never reaches a file leaves nothing to read, and there the
first column is all you get. A word your repository uses everywhere is not treated as a value at
all. The first real store this was run against, built from this project's own transcripts,
retired one record wrongly because a commit touched a line containing the word "delivered", and
the rule now ignores any word living in more than three tracked files unless the record spells
it like a name. Where the old value still appears somewhere in the repository, nothing is
retired. That is deliberate. And in the Spanish half of those sets the retirement is 10 of 10
while the answer is delivered 2 of 10. That gap is in retrieval, and "Where it is weak" explains
it.

Not retiring things is the other half of the job. It was never measured until a real
store made it obvious: run on six of this project's own transcripts, three of four sampled
retirements were wrong. One retired a note about gzip and zstd because a later message
described *running a test* on gzip and zstd.

The cause was a handful of words that are both a verb and a noun. "Migrations", "cambios",
"swap", "switch" read as announcements of a change, so any later sentence containing one of
them could retire an earlier decision it happened to share two words with. On fifteen pairs
built to contain exactly that shape, three of fifteen true decisions survived. After the fix,
twelve did. Of the three that still fail, two look like genuine changes that the test set
called unrelated. On two other held-out sets of pairs that are simply about different things,
fifteen of fifteen survive, before and after.

Some results were weaker, and they are published too. The same fact test on Codex gave 10 of 25
against 5 of 25, which is not a clear difference. Two features did not help and were removed or
turned off by default.

The comparison was run again with the decisions also implemented in the repository, the way a
real project works. **Muninn lost it, six times.** Six builds scored 6, 4, 7, 9, 5 and 6 out of
27 against claude-mem's 22, and the same cells said where the loss was: Muninn put the current
decision in front of the assistant in 27 cells out of 27, and the assistant used it in 4 to 9.
Five of those builds changed what Muninn *tells* the assistant. None of them moved it.

Two fixtures were withdrawn along the way, and one of those withdrawals did not save the
result. The first was withdrawn because nineteen of twenty-one failing cells reasoned from
commits Muninn cited that did not exist in the checkout they were standing in: the grid seeded
in one repository and ran the cells in another, which penalises exactly the checkable
provenance Muninn is built on. The fixture was fixed so every cited commit resolves, and
**Muninn still lost, 4 of 27 against 13 of 27.** That run was then voided too, for a defect it
found: the commit that removed a config directory (a rename) made Muninn retire six current
decisions and the conversations behind them.

What finally moved it was not better retrieval. Reading the competitor's own cells showed that
claude-mem does not deliver the decision at all — it delivers an index of every decision on
record, once per session, and the assistant then asks for the two it wants by id. An assistant
handed a filtered selection cannot tell a memory that holds nothing about a subject from a
query that missed it, and it acts on the first reading. Muninn now delivers the same thing
without a model: `[muninn:catalog]`, one line per record on the books, with `replaces #n` read
from the ledger and `conflict` where two active records disagree, plus `muninn show <id>` to
pull any of them in full.

On the shipped build, with the fixture validated by a no-memory control that scores 0 of 54:

| | replacement cells passed | retired value written |
|---|---|---|
| **Muninn** | **54 of 54** | **0 of 54** |
| claude-mem 13.24.23 | 25 of 54 | 10 of 54 |
| agentmemory 0.9.29 | 1 of 54 | 1 of 54 |
| no memory at all | 0 of 54 | 3 of 54 |

Exact Fisher p = 2.0 × 10⁻¹¹ against claude-mem and 4.4 × 10⁻³⁰ against agentmemory. Six runs,
54 cells an arm, the size and the threshold fixed before any cell ran, and the arm pinned to the
binary that ships before a cell of it ran. The competitor arms are not re-run: they are the same
fixture's own cells, each pinned to the version it was measured at. The build before the day of
defect fixes that produced this reads 48 of 54 on the same cells, and wrote the retired value
into 7 of them.

It cost context: Muninn occupies about 2.4 times claude-mem's share of the window, the worst
figure it publishes, and that is measured the way least favourable to us. Against the previous
build, on the same cells and one instrument, it is 0.876 [0.826, 0.891]: less than it was and
still more than the competitor's. Turning the per-prompt block off takes it to 1.8 and costs
about four answers in fifty, which is why it is not the default.

Where the decisions exist only in the conversation and never reach the code, the build that
ships reads **252 of 270 against claude-mem's 208 of 268** across five held-out wordings, each
generated after the fact, all ten grids in one run with both products seeding their own stores
from the same live sessions, and the decision rule registered before any cell ran (v43).
Exact Fisher **p = 1.7 × 10⁻⁷**.

| how the change was worded | Muninn | claude-mem | Muninn wrote the retired value | claude-mem did |
|---|---|---|---|---|
| bare values (`zstd`) | **40 of 54** | 28 of 53 | 0 | 0 |
| sentences | **54 of 54** | 32 of 53 | 0 | 3 |
| sentences, comparative | 54 of 54 | 50 of 54 | 0 | 1 |
| sentences with change verbs | 52 of 54 | 47 of 54 | 2 | 0 |
| sentences, mixed | 52 of 54 | 51 of 54 | 0 | 2 |
| **all five** | **252 of 270** | **208 of 268** | **2** | **6** |

Two wordings are won outright (bare values, p = 0.028; sentences, p = 3 × 10⁻⁸) and three are
ties; none is lost. Bare values used to be the loss to look at before installing anything, 18 of
54 against 31 of 53. They stopped being one without a model: the reply is now read on every
decision, and the catalogue lists what was said, newest first, as the latest word on a subject.
A language model on the write path was measured end to end to see whether it would add to that,
and it does not ([Z11]). Two claude-mem cells errored and could not be re-run because their
seeded stores were lost to a reboot; they are excluded, not counted as failures.

Where the decision also reaches the code, none of that applies: the value comes from the diff,
and that is the condition the 54 of 54 above was measured in.

The earlier comparison (claude-mem and agentmemory) gives every tool the same
decisions from the same real sessions, and nobody tells it which decisions were replaced.
Version 0.1.0 lost it to claude-mem (12 of 27 against 26 of 27). Version 0.2.0 was then tested on
new wording that no version had seen: it got 17 of 27, claude-mem 14 of 27 and agentmemory 9 of 27.
That difference is too small to call Muninn better, so it counts as a tie with both. Muninn's
answers also mentioned the replaced value more often (11 of 27 against 2 of 27 for claude-mem).

"Too small to call" is a limit of the test, not a finding about the tools. A second run of the
same comparison, done later for another reason, came out 23 of 27 against 19 of 27: the same
direction and the same four-answer gap. Put the two together and it is 40 of 54 against 33 of
54, which is still inside what chance produces about one time in five. To tell a difference that
size from nothing you would need about 204 cells per tool instead of 27, which is roughly eight
times the work. So it is a tie. The test was never big enough to find a difference this small in
either direction.

## Where it is weak

Turning a written rule into an enforced one covers a minority of what you write. Over 330
public `CLAUDE.md` and `AGENTS.md` files, about 93 % of the rules people write cannot be
enforced at the tool boundary at all: they are about style, judgement or process, and nothing
but the assistant reading them can honour them. Muninn tells you which of your rules are in
which half, and that is the limit of the feature.

For a decision that never reaches a file, noticing that you changed your mind is still the weak
half. Everything above about retired decisions assumes the decision got marked as retired in the
first place. Where the decision is in the code, the code now says when it stopped being true.
Where it is not (a release cadence, a review policy), the marking is a set of rules over the
words you typed, and on wording no version of Muninn had seen those rules caught 17 of 27
replacements against claude-mem's 14, which is a tie.

Word rules cannot close that gap: in 23 of the 30 held-out cases the old decision and the
message replacing it share no word. "HashiCorp Vault for production secrets" and "moving to AWS
Secrets Manager" share nothing a program can match on. Comparing meaning instead of words does
not work either. Measured on the product names alone over 42 development pairs it picked the
right pair **3 times**, where chance on a 42-way choice is one; an earlier six-pair reading said
5 of 6 and did not survive the larger one. Comparing whole sentences and comparing names alone
have both been measured, and neither works. The way past it was to stop reading sentences and
read the commits, which is the table above. That only helps for decisions that reach the code.
For a decision that never does, the marking is still a set of rules over the words you typed,
and they still miss.

The fact-recall numbers come from tasks we wrote, about this repository. 19 of 25 against 2 of 25
follows a rule written before the run, and it is still a measurement of our own tasks on our own
code. The Gate 3 grid was re-run on three outside projects. Gate 2 could not be, because its facts
live in this project's transcripts and do not move.

One of the ideas did not pay off. Muninn can anchor a record to a directory or to a code symbol,
so that touching the file brings the record back even when your words do not match it. Measured
against plain keyword search, that made no difference we can distinguish from zero
(+0.08, and the range around it runs from −0.08 to +0.25). It ships turned off, and what you get
is keyword search.

Muninn is a personal memory, and nothing about teams has been measured. Each person's store is
built from their own session transcripts, which live on their own machine, and `muninn init`
keeps the whole `.muninn/` folder out of version control. Two people on one repository therefore
have two memories, and neither sees what the other decided in conversation. The one channel they
do share is git history: a teammate's commit that takes a value out of the code retires the
decision that named it in your store too, and `docs/threat-model.md` §6 spells out that anyone
who can land a commit can retire a record. A shared store is deliberately left out: it would
need signed provenance and a way to admit writes, because a memory several people write to is a
documented attack surface `[C5]`. Versions before this one ignored only the database, logs and
state, so a `git add .` could commit the Markdown mirror: every record's words, retired ones
included, published with the code.

Retired records stay on disk in plain sight. Muninn mirrors every record to `.muninn/records/` as
Markdown, retired ones included and labelled as retired. Nothing hands them to the assistant, but
an assistant that greps the folder will find them. `MUNINN_NO_PROJECT` turns the mirror off.

Muninn takes more of your context window than the tools it was measured against. The catalogue
made that worse. On the part that is certainly the assistant's context it is now **2.6 times
claude-mem's room**, the worst figure this project publishes, against 1.6 to 2.1 before the
catalogue existed. Two things follow from the design: memory arrives when you ask something and
not only at the start, and the session now opens with a list of everything on record. Turning
the per-prompt half off (`muninn config prompt-delivery off`) takes it to 1.8 and costs about
four answers in fifty, both measured, which is why it is a switch and not the default. Cutting
the startup text instead was measured too and cost six answers in fifty-four, so the startup
text stays. The first version of this measurement said the opposite, and it was wrong: it
counted our own injection twice, because Muninn returns its context on standard output and the
hook record repeats it.

A question in one language does not reach a record in another. On the Spanish half of the
held-out sets the replaced decision is retired 10 times out of 10 and the new answer is
delivered 2 to 8 times out of 10, because the question's words are English and the record's
are Spanish, and the only bridge between them is a product name. Detection is not the problem
there; retrieval is, and nothing here fixes it.

The speed figures were measured on one machine. CI enforces the same limits on every run, so a
regression fails the build, but the figures are not a promise about your hardware.

There is no head-to-head against Mem0, Zep or Letta. Those have not been run on the same
harness, so there is no comparison to report and none is implied.

The assistant's own built-in memory could not be run either, and the reason cuts both ways.
Every cell of that comparison is a scripted, non-interactive session, and Claude Code's
automatic memory does not operate in one: asked whether it has a memory directory the assistant
answers "no memory", and a session told to remember something writes no file. A score for it
would have measured the session type, not the memory. What that does say, narrowly, is that
Muninn works where the built-in memory is not there. That is a statement about scripted sessions
on one version of the tool, not a claim that one memory is better than the other. The check is
one command and is in the repository, so a later version can be re-tested.

The full list of what is claimed, what is not, and the limits of each result is in
[`docs/claims.md`](claims.md). What Muninn deliberately does not do is in
[`docs/scope.md`](scope.md). The commands to reproduce every number are in
[`crates/muninn-bench/experiment/REPRODUCE.md`](../crates/muninn-bench/experiment/REPRODUCE.md).

## Install

You need Claude Code or Codex. You do not need Rust.

The repository is its own plugin marketplace, and both assistants read it from the same file,
`.claude-plugin/marketplace.json`.

```sh
# Claude Code (or /plugin marketplace add … inside a session)
claude plugin marketplace add ilien-dev/muninn
claude plugin install muninn@muninn

# Codex
codex plugin marketplace add ilien-dev/muninn
codex plugin add muninn@muninn        # then approve the hooks once in /hooks
```

`plugin/bin/` is not in the repository, so a fresh install has no binary. The SessionStart hook
runs [`plugin/scripts/session-start`](../plugin/scripts/session-start), which downloads the
release asset for the plugin's version into the plugin's `bin/` through
[`plugin/scripts/install.sh`](../plugin/scripts/install.sh). That script checks the published
sha256 and, when `cosign` is installed, the Sigstore signature. Later sessions find the binary
and only pay for one `exec`; the other hooks call it directly. A failed download leaves memory
off for that session and is retried at the next one. The script also keeps a link at
`~/.local/bin/muninn` pointing at the current binary, so `muninn` works by name in a terminal and
in Codex. Claude Code already puts the plugin's `bin/` on its own `PATH`. If `~/.local/bin/muninn`
is a regular file, it is someone's own install, and the script leaves it alone.

Each assistant reads its own manifest: Claude Code reads `plugin/.claude-plugin/plugin.json` and
`plugin/hooks/hooks.json`, and Codex reads `plugin/.codex-plugin/plugin.json`, which points at
`plugin/hooks/codex.json`. Codex hooks are shell commands and name Codex's tools (`apply_patch`),
so the file is separate. A test keeps it equal to what `muninn init --codex` generates.

Then, inside your own project, once per project and per machine:

```sh
muninn init          # or /muninn:init in Claude Code
muninn status        # check that everything is working
```

`muninn clean --yes` removes everything `init` added.

### What `init` does, and when to run it again

Hooks do nothing in a project until `.muninn/muninn.db` exists, so `init` is the opt-in. It
creates `.muninn/`, adds `.muninn/` to `.gitignore`, sets `autoMemoryEnabled: false` in the
project's `.claude/settings.json` (`--keep-native` skips that), and allows `muninn why`,
`muninn status` and `muninn show` there. Everything it touches outside `.muninn/` is recorded in
`.muninn/init.json`, which is how `clean` knows what to undo. Running it again changes only
what is missing.

A plugin update does not need it. The hooks follow the plugin to its new directory, the binary
for the new version arrives with the first session, and the store's schema is migrated by the
first hook that writes. Run it again only when a release note says so, for example when a
version adds a setting that `init` writes.

### Updates

- Claude Code: `/plugin` → Marketplaces → muninn → update, or enable auto-update there. From a
  terminal: `claude plugin marketplace update muninn && claude plugin update muninn@muninn`.
- Codex: `codex plugin marketplace upgrade` refreshes the marketplace and the installed plugin.

Restart the assistant after either. Codex stores each approval with a hash of the hook's
definition; in our test it survived a version bump that left the hooks unchanged.

### Without the plugin

`install.sh` also works on its own, for a machine where you would rather not install a plugin:

```sh
curl -fsSLO https://github.com/ilien-dev/muninn/releases/latest/download/install.sh
less install.sh          # read it before running it
sh install.sh            # puts muninn in ~/.local/bin
muninn init --codex      # writes .codex/hooks.json pointing at that binary
```

`init --codex` refuses to run from a plugin's binary: that path changes with every update, and
the Codex plugin already installs the same hooks.

### Publishing a release

Every version string lives in four places: `Cargo.toml`, both plugin manifests and the
marketplace. A marketplace install reads them from the default branch and downloads the release
tagged with that version, so they must agree before the tag is pushed:

```sh
scripts/bump-version.sh 1.0.1
git commit -am "Release 1.0.1" && git tag v1.0.1
git push && git push --tags
```

The `release` workflow fails if the tag and `Cargo.toml` disagree, and
`every_manifest_carries_the_binary_version` fails `cargo test` if a manifest is left behind.
Between the push and the end of the release workflow, a user who updates gets a version whose
binary is not published yet; their session runs without memory and the next one picks it up.

### From source

`plugin/bin/` is deliberately not in the repository. Build the binary into it, then add your
clone as a local marketplace. A local marketplace is used in place, so a rebuild reaches the
hooks at the next session:

```sh
git clone https://github.com/ilien-dev/muninn
cd muninn
cargo build --release -p muninn-cli
mkdir -p plugin/bin && cp target/release/muninn plugin/bin/
claude plugin marketplace add "$PWD" && claude plugin install muninn@muninn
```

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

The design and the reasons behind it are in [`design/ENGINE.md`](../design/ENGINE.md) and
[`research/CONCLUSION.md`](../research/CONCLUSION.md). Changes between versions are in
[`CHANGELOG.md`](../CHANGELOG.md).
