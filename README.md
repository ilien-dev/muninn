<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/muninn-dark.png">
    <img src="assets/muninn-light.png" alt="Muninn" width="480">
  </picture>
</p>

# Muninn

Muninn gives AI coding assistants (Claude Code and Codex) a memory that forgets on purpose.

Most memory tools search by resemblance: you ask a question, they return the notes that look most
like it. The trouble is that a decision you changed your mind about still looks exactly as
relevant as the one that replaced it. So the assistant gets both, and picks one.

Muninn does not score notes. It records decisions — this topic, this value, said by this person,
at this point in the conversation — and when you decide something else about the same topic, the
earlier decision stops existing for the assistant. Not ranked lower. Gone from the list. It stays
in your history, and you can always ask to see it.

Everything runs on your computer. There is no server, no account, no cloud service and no AI
model inside Muninn. It is one small program that stores its notes in a local file inside your
project.

## What it does

**It remembers what you decided, and only while it is still true.** Decisions, corrections you
made, rules you stated, approaches that failed. When a later question touches one of them, the
assistant gets your original words and a pointer to where you said them.

**It turns written rules into enforced ones.** A line in `CLAUDE.md` or `AGENTS.md` such as "never
edit the migrations folder" can become a permission setting the assistant's tool actually obeys,
rather than a sentence it may or may not follow. Nothing changes until you have seen the proposed
change and said yes.

## How it works

Three steps. Nothing in them needs you to do anything.

**1. It writes down what you decided.** When a session ends, Muninn reads the conversation and
keeps the parts that are decisions, corrections, rules or dead ends. Each one is stored as a small
card: the topic, the value, where it came from, and the exact spot in the transcript where you
said it. It keeps your words, not a summary of them — so nothing is quietly reworded into
something you did not say.

**2. It retires what you replaced.** If you later decide something else about the same topic, the
earlier card is marked as superseded. The card is not deleted: it stays in your history with a
label saying what replaced it. It simply stops being something the assistant can be shown.

**3. It hands over only what is current.** On each turn, Muninn looks at what you are writing and
at which files you are touching, finds the cards that relate to it, and passes them to the
assistant as *evidence* — never as an order — with a hard cap of 700 tokens. It takes a few
thousandths of a second, and it never calls an AI model to do it. Same store, same question, same
answer, every time.

The part worth pausing on is step 2, because it is the one other tools cannot copy by tuning. In a
search-by-resemblance memory, a retired note is still a note; the best you can do is hope it ranks
lower. In Muninn the queries that feed the assistant physically cannot reach a retired card — they
read from a filtered view of the database, and the code that formats what the assistant sees can
only be built from that view. A retired card is not unlikely to appear. It is unable to.

## An example

Monday, you tell the assistant:

> for the transport compression codec we go with gzip

Thursday, you change your mind:

> change of plan — the transport compression codec is now zstd

Two weeks later, in a new session, you ask the assistant to write up the project's compression
policy. It has no memory of either conversation, so it asks its memory tool.

A search-by-resemblance memory finds both notes — they are about the same topic, so they score
about the same — and the assistant has to guess which one is current. Muninn hands it exactly one
card:

```
[muninn:decision] #2 · a1b2c3d4 · origin: user_said · trust 3
user: change of plan — the transport compression codec is now zstd.
  evidence: ~/.claude/projects/your-project/a1b2c3d4-….jsonl:48213
```

Reading that block: it is a **decision** (not a guess, not a summary); `#2` is its number;
`origin: user_said` and `trust 3` mean you said it yourself, which is the highest of the four
levels; and `evidence:` points at the exact line of the transcript where you said it, so the
assistant — or you — can go and check. Cards Muninn is less sure about arrive labelled as hints
rather than facts. The full vocabulary is in [`docs/format.md`](docs/format.md).

The gzip decision has not been destroyed. Ask for it and it is there, labelled:

```sh
muninn why "compression codec"
```

```
[muninn:decision] #1 · policy.compression is · origin: user_said · trust 3 · 2026-08-31T09:41:00Z · RETIRED (superseded)
user: for the transport compression codec we go with gzip.
```

That is the whole idea: the history is complete, and what reaches the assistant is only what still
holds.

## Why not just a normal memory tool?

| when this happens | a search-by-resemblance memory | Muninn |
|---|---|---|
| you changed your mind about something | returns the old note and the new one, and ranks them by how well they match your words | the old one is not in the list at all |
| you ask what you decided *not* to do | has no way to express a negative; you get whatever mentions the topic | dead ends are a kind of card, and are searched as one |
| a rule was revoked | the revoked rule is still text in the store, and still retrievable | revoked cards cannot reach the assistant; you see them only when you ask |
| what it costs you per turn | an embedding lookup, often a model call, and tokens you don't control | a few thousandths of a second, no model, capped at 700 tokens |

Two findings from the published literature (**other people's measurements, not ours** — the
evidence ids point into [`research/00-evidence-log.md`](research/00-evidence-log.md)):

- On questions of the form "is this still true?", "is this the complete set?" and "what did we
  decide against?", vector search answered 6–27 % correctly, against 98–100 % for typed records
  with explicit supersession links — at the same token cost and the same relevance `[K6]`.
- Of five memory systems loaded with a revoked policy and its replacement, **none enforced the
  revocation by default**: the revoked fact came back, outranked its own replacement, and led the
  agent to the unsafe action `[K7]`. That finding is about those five systems; it is not a claim
  about every memory tool that exists.

What Muninn does *not* claim is just as short, and it is on the same page as everything else:
[`docs/claims.md`](docs/claims.md). The honest summary is that serving only current decisions is
settled, and *noticing* that you changed your mind is hard — on wording no version had seen,
Muninn scored 17 of 27 against claude-mem's 14 of 27, which is a tie, not a win. What is
deliberately left out is in [`docs/scope.md`](docs/scope.md).

## Does it work?

Every result below comes from a test whose rules were written and timestamped before it ran.
The raw data is in this repository, and anyone can re-run the tests.

One of the claims is not a number, and it is the important one. **A retired decision is never
handed to the assistant.** That is not a success rate, it is a property of how the code is built:
the queries that feed the assistant read from a filtered view of the database, and the code that
formats what it sees cannot be constructed from anything else. A test drives the real hooks 200
times per run — including with the database corrupted, the schema unreadable and the clock moved
backwards — and fails if a retired value ever appears in the output. It was checked the other way
round too: undo the filter and the test goes red naming the value that leaked.

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

The full list of what is claimed, what is not, and the limits of each result is in
[`docs/claims.md`](docs/claims.md). The commands to reproduce every number are in
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
muninn why "why did we choose exponential backoff"   # what was decided, when, and what replaced it
muninn revoke 142 --reason "no longer true"          # hide a note by hand (it stays in the history)
muninn export --all                                  # every note, as a file you can read
muninn compile && muninn apply                       # turn written rules into enforced ones
```

## For developers

The code is a Rust workspace. The storage layer is SQLite with full-text search.

```
crates/muninn-core      storage, search, filtering
crates/muninn-cli       the Muninn program and its hooks
crates/muninn-capture   reads conversations and git history
crates/muninn-compile   turns written rules into permission settings
crates/muninn-embed     optional local similarity search (never used while answering the assistant)
crates/muninn-symbols   map of the code's functions and types
crates/muninn-why       answers `muninn why`
crates/muninn-bench     speed tests and the experiments behind every number
plugin/                 Claude Code plugin
codex/                  Codex setup
```

Tests:

```sh
cargo test --workspace --features exact-tokens
cargo run --release -p muninn-bench -- perf --strict
```

The design and the reasons behind it are in [`design/ENGINE.md`](design/ENGINE.md) and
[`research/CONCLUSION.md`](research/CONCLUSION.md). Changes between versions are in
[`CHANGELOG.md`](CHANGELOG.md).

## About the name

<img src="assets/muninn-symbol.png" alt="" width="96" align="right">

In Norse myth, Odin keeps two ravens. They fly out over the world at dawn and come back at supper
to tell him what they saw. Huginn is thought. Muninn is memory. In the *Grímnismál*, one of the
poems of the Poetic Edda, Odin admits that he fears Huginn may not return — but that he fears more
for Muninn.

It is an odd thing for a god to say, and it is exactly right. Thought can be done again. A memory
that does not come back is gone.

A coding assistant is Huginn. It thinks quickly, it ranges widely, and every morning it starts
from nothing. Muninn is the one that comes back carrying what you decided.

The mark is a raven inside a broken ring: the ring is a decision that no longer holds. Kept,
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
