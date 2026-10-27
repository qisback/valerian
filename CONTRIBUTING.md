# Contributing to Valerian

**I am not accepting contributions at the moment.** I develop Valerian on an internal
repository with an extensive internal CI system, and everything is built and tested there
before it reaches GitHub. The GitHub repository is a published mirror. A pull request
would have to be pulled back into the internal repository, tested on that CI, then rebased
and pushed out again, so for now I close pull requests without merging them.

**Bug reports and feature requests are welcome** as GitHub issues; see [Reporting bugs](#reporting-bugs) for what to include.

The rest of this page describes how Valerian is built and checked, for anyone who wants to
build it from source or understand how it is worked on.

## Building and checking locally

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
VALERIAN_GPU_TESTS=software cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check
cargo xtask docs --check
cargo xtask reuse-check
zensical build --strict --clean
```

CI runs one job per check on the Linux lane (`fmt`, `clippy`, `test`, `doc`,
`deny`, `reuse`, `book`), so a green local run is the minimum. Two more lanes run
on self-hosted runners and never on pull requests (a fork's code must not reach
them): `windows` (push to `main`, nightly, on the self-hosted Windows runner, GPU
tests on WARP) and `gpu-nightly` (nightly, `VALERIAN_GPU_TESTS=hardware` on the
NVIDIA card on the build server). The GPU lane is skipped until the GPU runner serves this
repository: it is guarded by the repository variable `VALERIAN_GPU_RUNNER`.

## Documentation is part of the change

Docs are produced with every change and must stay true. The book lives in
`docs/book` ([Zensical](https://zensical.org/), configured by `zensical.toml`); `docs/ARCHITECTURE.md` is included in it.
Every pull request:

1. **Re-checks all docs**, not only the page you expect to be affected. Read the
   README, `CONTRIBUTING.md`, `docs/ARCHITECTURE.md` and the book for anything
   your change makes untrue: a flag, a default, what runs on the GPU, a "not
   yet implemented" note.
2. **Updates the affected pages** in the same pull request. A new option or
   subcommand needs its explanation, not just its code. Say plainly what does
   not work yet, and quote performance figures only if measured, with the
   hardware and the commit.
3. **Updates `CHANGELOG.md`** under *Unreleased*.
4. **Runs `cargo xtask docs`** and commits the result. It regenerates the CLI
   reference (from the clap definition in `crates/valerian-cli/src/lib.rs`)
   under `docs/book/src/reference/`. CI runs `cargo xtask docs --check` and
   fails when the committed file differs. Never edit it by hand: change the
   doc comments it comes from.
5. **Builds the book** with `zensical build --strict --clean`. Strict mode fails on
   a broken link, image or include. Install it once with
   `python3 -m venv .venv && .venv/bin/pip install -r docs/requirements.txt`. A new page
   goes into the `nav` list in `zensical.toml` (`cargo xtask docs --check` checks this).

The pull request template has these as checkboxes. Documentation is written in
British English, with plain, friendly prose for users and precise prose for
developers.

## Copyright and licence headers

Contributors keep the copyright in their own work; the terms for outside
contributions (any contributor agreement) are not settled. Valerian is GPL-3.0-or-later with the additional terms in
[ADDITIONAL-TERMS.md](https://github.com/qisback/valerian/blob/main/ADDITIONAL-TERMS.md)
(GPL section 7(b) and 7(c): the author attribution and the dedication are
preserved, and modified versions are marked as changed). By contributing you
agree your work is released under that licence and those terms. The project
follows the [REUSE specification](https://reuse.software/spec/):

- Every source file (Rust, WGSL, Python, shell, PowerShell, workflow YAML) starts
  with three comment lines, in that language's comment syntax (after a `#!` line
  in scripts):

  ```
  // SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
  // SPDX-License-Identifier: GPL-3.0-or-later
  // See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
  ```

- If you substantially change a file, add your own line above or below the
  existing one, for example `SPDX-FileCopyrightText: 2027 Your Name`. Do not
  remove anyone else's line.
- Documentation, images and configuration that cannot carry a header are covered
  by an annotation in `REUSE.toml`. Third-party files keep their own copyright
  and licence there.
- `cargo xtask reuse-check` fails when a file has neither, and CI runs it.

## Rules of the codebase

- Every GPU kernel has a CPU reference implementation, and a test that the two
  agree within a stated tolerance. The CPU path is the specification.
- Image maths is tested on synthetic data with known answers (a known shift, a
  known rotation, injected noise, hot pixels and satellite trails), not only on
  "looks right" samples.
- Real image data is never committed. Tests that need it read it from a path in
  an environment variable and skip when it is unset.
- No `unsafe` outside crates that explicitly allow it.
- New dependencies must pass `cargo deny` (GPL-compatible licences only).
- Code shared with Helia is a pinned git dependency for now; see
  issue #4 for what
  should move to a shared workspace.
- British English in user-facing text and documentation.

## Reporting bugs

Include the OS, GPU and driver, `valerian --version`, and if possible a small set
of frames that shows the problem.
