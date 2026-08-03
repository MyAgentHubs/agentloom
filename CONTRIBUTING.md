# Contributing to AgentLoom

Thanks for being here. AgentLoom is built by a very small team, so this
document is blunt about how we work — the goal is that contributions you spend
time on actually land.

## Talk first, code second

Reviewing a patch costs us more than writing one. A pull request that arrives
with no prior conversation is the most expensive kind of contribution: we have
to reconstruct the problem, guess at the intent, and decide whether the
approach fits a roadmap you can't see.

So we've inverted the usual order:

1. Open an issue describing the problem.
2. Wait for a maintainer to label it `accepted`.
3. Then send the pull request, linking that issue.

**Pull requests that don't reference an `accepted` issue will be closed** —
usually quickly, and sometimes without a detailed explanation. That is not a
judgment on your code. It means we haven't agreed on the problem yet, and
agreeing on the problem is the cheap part.

Two things never need an issue: fixing a typo or a broken link, and correcting
a factual error in the docs. Just send those.

## What to expect from us

We'd rather set expectations than have you guess:

- We review in batches, usually once or twice a week.
- **One open pull request per contributor at a time.** We enforce this in review.
  Finish or close the one you have before opening the next.
- Large refactors, dependency swaps, and new abstractions are almost always
  declined unless we asked for them in an issue first — not because they're
  wrong, but because they're expensive for us to verify.
- We may close a stalled pull request rather than let it rot. Reopening is
  cheap; ask and we'll reopen.
- We don't run a bug bounty and we don't participate in Hacktoberfest.

## Using AI to contribute

We build an AI coding agent. Of course you can use one. We do.

What we ask is that you own the output.

**Disclose it.** The pull request template has a checkbox. Tell us what the
tool did and what you checked yourself. Nobody has ever been rejected for
ticking that box.

**Write your own prose.** Pull request descriptions, issue reports, and review
comments must be written by you. This rule is about the writing, not the code.
A well-formed explanation that turns out to be confidently wrong is the single
most expensive thing you can send a maintainer: it costs us the entire careful
read before we discover there's nothing underneath it. Two badly-worded honest
sentences beat six polished invented ones. Generated prose gets the thread
closed.

**Run it before you send it.** Paste the actual output of the checks below into
your pull request. Not "tests pass" — the output.

**Read every line you're submitting.** If you can't explain why a hunk is
there, it isn't ready. Same principle the Linux kernel settled on: a human
signs off, and that human is accountable for the whole patch.

There's an [`AGENTS.md`](AGENTS.md) at the repository root with these rules in
a form your agent can read. Point your tool at it before you start.

## Setting up

You'll need [Rust](https://rustup.rs) (stable), Node.js 20+, and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your
platform.

```bash
git clone https://github.com/MyAgentHubs/agentloom.git
cd agentloom/harness-agent
cargo build --release

mkdir -p ../app/src-tauri/binaries
cp target/release/myagent ../app/src-tauri/binaries/myagent-$(rustc -vV | sed -n 's/^host: //p')

cd ../app
npm install
npm run tauri dev
```

## Checks to run before you push

Frontend and app shell, from `app/`:

```bash
# First stage the sidecar required by the Tauri build script
cargo build --release --locked --manifest-path ../harness-agent/Cargo.toml
triple="$(rustc -vV | sed -n 's/^host: //p')"
mkdir -p src-tauri/binaries
cp "../harness-agent/target/release/myagent" "src-tauri/binaries/myagent-${triple}"

npm run typecheck      # vitest does NOT typecheck — run this too
npm test
npm run format:check
cargo test --no-fail-fast --manifest-path src-tauri/Cargo.toml
```

Engine, from `harness-agent/`:

```bash
cargo test --no-fail-fast
cargo fmt --check
```

Three things that bite people:

- `npm test` does not check types. A change can be green in vitest and still
  fail the build. Always run `npm run typecheck` as well.
- `cargo test` without `--no-fail-fast` stops at the first test binary that
  fails, so a single broken integration test hides every failure behind it.
  `cargo test --lib` is worse: it skips everything under `tests/` without
  saying so.
- Only format the files you touched. Repository-wide `prettier --write .` or a
  bare `cargo fmt` will bury your actual change in hundreds of unrelated lines
  and we'll ask you to redo it.

## Contributor License Agreement

AgentLoom is licensed under AGPL-3.0. Before your first pull request can be
merged, you'll be asked to sign a lightweight CLA — a bot posts a link in the
pull request and you click once. It confirms you have the right to contribute
the code and lets us keep future licensing options open.

The name and logo are handled separately; see [`TRADEMARK.md`](TRADEMARK.md).

## Reporting a vulnerability

Don't open an issue. See [`SECURITY.md`](SECURITY.md).

## Code of conduct

Participation is governed by our [Code of Conduct](CODE_OF_CONDUCT.md). Be
decent to each other.
