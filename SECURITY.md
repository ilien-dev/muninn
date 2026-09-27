# Security

## Reporting a problem

Please don't open a public issue for a vulnerability. Report it privately from the
repository's **Security** tab: choose **Report a vulnerability**. Only the maintainer sees
the report.

Say what you found, the version (`muninn --version`), your operating system and assistant
(Claude Code or Codex), and the steps that show it.

## What counts

Muninn reads your assistant's conversations and puts parts of them back into later ones, so
these matter most:

- a way for text from a file, a web page or a tool's output to reach the assistant as an
  instruction instead of as quoted memory.
- a memory that was retired being served again.
- anything that makes a hook run a command other than the Muninn binary.
- a downloaded binary that is used without its checksum matching.
- conversation text leaving your machine. The Muninn binary makes no network request. The
  plugin's scripts download two things: the release binary, and the embedding model if you ask
  for it.

[docs/threat-model.md](docs/threat-model.md) lists each of these, the defence in place, and
what is out of scope.

## Supported versions

Fixes go into the latest release. Update the plugin to receive them.
