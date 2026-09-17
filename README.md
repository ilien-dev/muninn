<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/muninn-dark.png">
    <img src="assets/muninn-light.png" alt="Muninn: What still holds, returns." width="480">
  </picture>
</p>

# muninn

Muninn gives AI coding assistants (Claude Code and Codex) a memory that stays up to date.

When you work with an assistant over many sessions, you make decisions along the way: "we use
Postgres", "don't touch the payments folder", "we switched from gzip to zstd". A memory that only
saves notes keeps serving the old decision next to the new one when something changes, and the
assistant may follow the wrong one. Muninn keeps a history of what was decided and
notices when a decision has been replaced. The assistant only sees what is true now.

Everything runs on your computer. There is no server, no account, no cloud service and no AI
model inside Muninn. It is one small program that stores its notes in a local file inside your
project.

## What it does

After each session, Muninn reads the conversation and keeps the parts worth remembering:
decisions, corrections you made, rules you stated and approaches that failed. When a later question touches one of them, it hands the assistant the
original words and says where they came from.

If you later say "actually, let's use zstd", the gzip note is kept in the history but is no longer
shown to the assistant.

Rules in `CLAUDE.md` or `AGENTS.md`, such as "never edit the migrations folder", can become
permission settings that the assistant's tool actually enforces. Nothing is changed until you have seen the proposed change.

Each check takes a few thousandths of a second, and what it adds to a prompt is capped at 700
tokens (a few hundred words).

## Does it work?

Every result below comes from a test whose rules were written and timestamped before it ran.
The raw data is in this repository, and anyone can re-run the tests.

| test | with Muninn | without memory |
|---|---|---|
| The assistant needs a fact that only an earlier session contains (tasks on this project) | 19 of 25 tasks solved | 2 of 25 |
| The assistant is given notes in which the replaced decisions are already marked, on three open-source projects | 161 of 162 tasks correct | 3 of 162 |
| Important rules survive when the conversation is compressed to save space | 100 of 100 compressions | not measured here |

Some results were weaker, and they are published too. The same fact test on Codex gave 10 of 25
against 5 of 25, which is not a clear difference. Two features did not help and were removed or
turned off by default.

The comparison with other memory tools (claude-mem and agentmemory) is still running. In it,
every tool learns the same decisions from the same real sessions, and nobody tells it which
decisions were replaced. Version 0.1.0 lost that comparison to claude-mem (12 of 27 against
26 of 27). Version 0.2.0 tied it (27 of 27 against 26 of 27) on the same sessions, but those
sessions were used while improving it, so that result is not taken as proof. A second round with
new wording that no version has seen is the one that counts, and its results will be published
whatever they are.

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
crates/muninn-cli       the muninn program and its hooks
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

## License

AGPL-3.0-only
