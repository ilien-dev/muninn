# Contributing to Muninn

## License and CLA

Muninn is licensed under the GNU Affero General Public License, version 3 only
([`LICENSE`](LICENSE)), with the additional attribution terms in
[`NOTICE`](NOTICE). By contributing, you agree that your work is published
under those terms.

Before a pull request can be merged, you must sign the
[Contributor License Agreement](CLA.md) once. The CLA check on the pull request
tells you how: post the comment it asks for. You keep the copyright in your
contribution. The CLA lets the maintainer also offer Muninn under other terms,
such as a hosted edition, and commits the maintainer to keep every accepted
contribution public under the AGPL.

## Before you open a pull request

CI runs these commands, and a change is finished when all of them pass:

```sh
cargo fmt --all && cargo clippy --workspace --all-targets --features exact-tokens -- -D warnings
cargo test --workspace --features exact-tokens
MUNINN_FAULT_REPS=200 cargo test -p muninn-cli --release --test fault
cargo run --release -p muninn-bench -- perf --strict
cargo run --release -p muninn-cli --features exact-tokens -- init --check-budget
```

The design is fixed by [`research/CONCLUSION.md`](research/CONCLUSION.md) and
[`design/ENGINE.md`](design/ENGINE.md). A change that overturns a recorded
decision needs a new measurement. Every number in the documentation is
measured or cited by evidence id.
