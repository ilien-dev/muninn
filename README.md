<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/muninn-dark.png">
    <img src="assets/muninn-light.png" alt="Muninn" width="480">
  </picture>
</p>

# Muninn

**A memory for Claude Code and Codex that keeps up when you change your mind.**

Every time you open a new session, your AI assistant starts from zero, with no idea that last
week you picked a database, agreed on a folder structure, or told it never to touch a certain
file. So you explain it all again, or it guesses.

Memory tools fix the forgetting, but most of them bring a new problem. They remember
*everything*, including the decisions you have since changed. Ask about the database and the
assistant gets the old choice and the new one side by side, and has to guess which is current.
Sometimes it guesses wrong, and you find the old choice back in your code.

Muninn remembers what you decided, and when you change your mind, it puts the old decision away.
The assistant only sees what is true now. The old one is still in your history if you ever want
to look.

## A quick example

On Monday you tell the assistant:

> Let's use gzip to compress the files we send.

On Thursday you change your mind:

> Change of plan, let's use zstd instead.

Two weeks later, in a brand-new session, you ask it to write up how the project compresses
files. The assistant remembers neither conversation, so it checks its memory. A typical memory
tool hands it both messages. Muninn hands it only Thursday's, and the assistant writes zstd.

## Test results

We tested it against two popular memory tools, claude-mem and agentmemory. Every tool got the
same conversations, and the assistant was then given tasks that depended on a decision that had
later been changed.

- **When you change your mind in conversation**, the assistant used the up-to-date decision in
  **252 of 270 tasks (93%)** with Muninn, against **208 of 268 (78%)** with claude-mem. We tried
  five different ways of phrasing the change, and Muninn was never behind.
- **When the change also shows up in your code**, the assistant got it right in **54 of 54**
  tasks with Muninn, 25 of 54 with claude-mem and 1 of 54 with agentmemory.
- **An old decision never sneaks back in.** With Muninn the assistant wrote a replaced value into
  its work 0 times out of 54. With claude-mem it did 10 times.

An early measurement also points to lower costs: on those same tasks the assistant took about
half as many steps with Muninn, and each task cost about 40% less. That figure has not been
confirmed by a test of its own yet, so read it as a first sign.

Each test had its rules written down before it ran, and all the raw results are in this
repository, so anyone can check them. The full comparison, including the tools we have not
tested yet, is in [docs/why-muninn.md](docs/why-muninn.md).

## What else it does

Once it is set up it works in the background, and you keep talking to your assistant as usual.
It keeps what you actually said rather than a summary of it, so nothing gets rewritten into
something you never meant. Everything stays on your computer: Muninn has no account and no cloud
service, and it never sends your conversations anywhere. It adds a few thousandths of a second
to each message.

It can also turn a rule from your project notes, such as "never edit the migrations folder", into
a setting that blocks the action instead of hoping the assistant remembers. It shows you the
change first and does nothing until you say yes.

## Limits

- Muninn uses a bit more of the assistant's attention. Assistants can only keep so much text in
  mind at once. Muninn's reminders take about 2.4 to 2.6 times the room claude-mem's do. You can
  switch the per-message reminders off, at the cost of a few more wrong answers.
- It notices a change best when you write a sentence. "Let's switch to zstd" is easy to catch,
  while a reply that is just the word "zstd" is harder, though Muninn now wins that case too.
- Languages don't mix well. If you ask in English about something you decided in Spanish,
  Muninn may not find it.
- It is a personal memory. On a team, each person gets their own, built from their own
  conversations, and a `git push` does not share it with anyone. Muninn keeps its folder out of
  your repository, so your conversations are not published along with your code.
- It works with Claude Code and Codex only, for now. Other assistants are planned for later.

## Install

You need Claude Code or Codex. You do not need to know how to program. Installing is done once
per computer; after that, you turn Muninn on in each project where you want memory.

### Claude Code

Type these two lines in Claude Code, one at a time:

```
/plugin marketplace add ilien-dev/muninn
/plugin install muninn@muninn
```

Restart Claude Code. The first session downloads Muninn itself, about 13 MB, and checks the
download before using it.

### Codex

Run these in a terminal:

```sh
codex plugin marketplace add ilien-dev/muninn
codex plugin add muninn@muninn
```

Open Codex, type `/hooks` and approve Muninn's hooks. Codex runs no plugin hook until you do.
The first session after that downloads Muninn, the same way as in Claude Code.

## Setting up a new project

Muninn stays silent in a project until you turn it on there. Do it once per project, on each
computer you work from: the memory lives on your machine, not in the repository, so a fresh
clone starts without it.

**In Claude Code**, open the project and type:

```
/muninn:init
```

**In Codex** there is no command for this, so you ask Codex to do it. Open Codex in the project
folder, paste this message and send it:

```
Run `muninn init` in this project, then `muninn status`. If `muninn` is not found, use `~/.local/bin/muninn` instead. Show me what both commands print.
```

Codex may ask your permission before running them; say yes. When it's done, the last line it
shows starts with `MUNINN` and says `GREEN`.

If you prefer the terminal, run this inside the project folder instead:

```sh
~/.local/bin/muninn init
```

That file appears after your first Codex session with the plugin installed. If it isn't there
yet, open Codex once, close it, and try again.

To check it in Claude Code, type `/muninn:status`: it prints a line starting with `MUNINN` that
says `GREEN`. Memory starts filling from that conversation on. If you use both assistants on the
same project, one `init` covers both.

What `init` changes in the project:

- It creates the `.muninn/` folder, where the memory is kept, and adds it to `.gitignore`, so
  your conversations never end up in a commit.
- It turns off Claude Code's own memory for the project. `muninn init --keep-native` leaves it
  on.
- It lets the assistant run `muninn why`, `muninn status` and `muninn show` without asking you
  each time.

Running it twice is harmless. To remove Muninn from a project, run `muninn clean --yes`: it
undoes exactly what `init` changed.

## Updating

- Claude Code: open `/plugin`, choose the muninn marketplace and update it, or turn on
  auto-update there once.
- Codex: run `codex plugin marketplace upgrade`.

Restart the assistant afterwards. You don't run `muninn init` again: the first session after an
update fetches the new version, and the memory you already have stays.

## Using it

You don't need to do anything. Muninn listens while you work. If you are curious, you can ask it
what it knows:

```sh
muninn why "why did we pick zstd"   # what stands now, and what it replaced
muninn status                        # is everything healthy
```

## For the technically curious

- [How Muninn works, and how well](docs/technical-overview.md): the design, every measurement,
  where it is weak, and how to build it from source.
- [Why Muninn](docs/why-muninn.md): the comparison with other memory tools.
- [What we claim and what we don't](docs/claims.md): every result with its limits.
- [What Muninn deliberately leaves out](docs/scope.md).
- [Changes between versions](CHANGELOG.md).

## About the name

<img src="assets/muninn-symbol.png" alt="" width="96" align="right">

In Norse myth, the god Odin keeps two ravens. Every morning they fly out over the world, and every
evening they come back to tell him what they saw. Huginn is thought; Muninn is memory. In one of
the old poems, Odin says he worries that Huginn may not come back, and that he worries more for
Muninn.

That always struck me as the right way round: a thought can be had again, and a memory that does
not come back is gone.

Your AI assistant is Huginn. It thinks fast, it covers a lot of ground, and every morning it
starts from nothing. Muninn is the one that comes back carrying what you decided.

## License

Muninn is © 2026 ilien and free to use under the
[GNU Affero General Public License, version 3 only](LICENSE), with the extra terms in
[`NOTICE`](NOTICE). In short:

- Anyone may use, change, host and sell Muninn, including as a cloud service.
- Anyone who shares a changed version, or offers one to people over a network, must publish its
  full source code under the same license.
- Every copy must keep the notice "Based on Muninn by ilien - https://github.com/ilien-dev/muninn",
  and a changed version must say it was changed.

Want to contribute? There is a one-time [Contributor License Agreement](CLA.md); see
[CONTRIBUTING.md](CONTRIBUTING.md).
