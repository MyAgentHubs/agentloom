# AgentLoom app (`app/`)

**English** · [简体中文](README.zh-CN.md)

The desktop app: a Tauri 2 + React / TypeScript frontend with a Rust backend (`src-tauri/`). It embeds the myagent engine from `harness-agent/` and runs it as a sidecar alongside the app.

## Local development

From `app/`, first build the sidecar and put it in place (the Tauri build script needs it), then start the app:

    cargo build --release --locked --manifest-path ../harness-agent/Cargo.toml
    triple="$(rustc -vV | sed -n 's/^host: //p')"
    mkdir -p src-tauri/binaries
    cp ../harness-agent/target/release/myagent "src-tauri/binaries/myagent-${triple}"
    npm ci
    npm run tauri dev

## Checks

    npm run typecheck        # TypeScript type check (vitest does not type-check; run it separately)
    npm test                 # frontend tests (Vitest)
    npm run format:check     # format check
    cargo test --no-fail-fast --manifest-path src-tauri/Cargo.toml   # Rust backend tests

## More

See the root `README.md` for the project overview, `CONTRIBUTING.md` for the contribution flow, and the root `AGENTS.md` for the full checklist and collaboration rules.
