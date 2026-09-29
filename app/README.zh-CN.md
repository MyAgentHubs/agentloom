# AgentLoom 应用（app/）

[English](README.md) · **简体中文**

桌面应用：Tauri 2 + React / TypeScript 前端 + Rust 后端（`src-tauri/`）。内置 `harness-agent/` 里的 myagent 引擎，作为 sidecar 随应用一起运行。

## 本地开发

在 `app/` 下，先构建并放好 sidecar（Tauri 构建脚本需要它），再起应用：

    cargo build --release --locked --manifest-path ../harness-agent/Cargo.toml
    triple="$(rustc -vV | sed -n 's/^host: //p')"
    mkdir -p src-tauri/binaries
    cp ../harness-agent/target/release/myagent "src-tauri/binaries/myagent-${triple}"
    npm ci
    npm run tauri dev

## 检查

    npm run typecheck        # TypeScript 类型检查（vitest 不查类型，必须单独跑）
    npm test                 # 前端测试（Vitest）
    npm run format:check     # 格式检查
    cargo test --no-fail-fast --manifest-path src-tauri/Cargo.toml   # Rust 后端测试

## 更多

项目概览见根目录 `README.md`，贡献流程见 `CONTRIBUTING.md`，完整检查清单与协作规则见根目录 `AGENTS.md`。
