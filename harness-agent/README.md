# myagent — the AgentLoom engine

`myagent` is a provider-agnostic agent engine written in Rust. It turns any
OpenAI- or Anthropic-compatible chat API (DeepSeek, GLM, Kimi, Qwen, a local
model, …) into a full coding agent: tool calling, plan mode, file checkpoints,
web search, MCP, and a replayable event journal.

AgentLoom ships it as a bundled sidecar, but it also works standalone:

```bash
cargo build --release
DEEPSEEK_API_KEY=... ./target/release/myagent run --provider deepseek "your task"
```

Point it at any compatible endpoint with `{PROVIDER}_API_KEY` /
`{PROVIDER}_BASE_URL` / `{PROVIDER}_MODEL`. See `myagent --help` for run,
plan, resume, shell and MCP subcommands.

Build instructions for the full desktop app are in the
[repository README](../README.md).
