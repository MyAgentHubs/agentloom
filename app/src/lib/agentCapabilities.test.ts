import { describe, expect, it } from "vitest";
import { hasLeadCapability } from "./agentCapabilities";

describe("hasLeadCapability", () => {
  it("native claude enabled -> true", () => {
    expect(
      hasLeadCapability({
        enabled: true,
        provider: "claude",
        access: "native",
      }),
    ).toBe(true);
  });

  it("native claude disabled -> false", () => {
    expect(
      hasLeadCapability({
        enabled: false,
        provider: "claude",
        access: "native",
      }),
    ).toBe(false);
  });

  it("enabled deepseek native -> false", () => {
    // native 但不是 claude：既不是 native claude 也不是 borrow，不支持。
    expect(
      hasLeadCapability({
        enabled: true,
        provider: "deepseek",
        access: "native",
      }),
    ).toBe(false);
  });

  it("enabled claude borrow -> true", () => {
    // L1b：borrow access 即支持，与 provider 无关（含 provider=claude 的 borrow 配置）。
    expect(
      hasLeadCapability({
        enabled: true,
        provider: "claude",
        access: "borrow",
      }),
    ).toBe(true);
  });

  it("enabled deepseek borrow -> true", () => {
    // L1b 核心场景：借壳 DeepSeek/GLM 经 claude 二进制接入，可以当队长。
    expect(
      hasLeadCapability({
        enabled: true,
        provider: "deepseek",
        access: "borrow",
      }),
    ).toBe(true);
  });

  it("disabled borrow -> false", () => {
    expect(
      hasLeadCapability({ enabled: false, provider: "glm", access: "borrow" }),
    ).toBe(false);
  });

  it("enabled codex native -> false", () => {
    // codex native：L1 spawn 接不住，与后端 lead_engine_for_profile 的 engineNotSupported 一致。
    expect(
      hasLeadCapability({ enabled: true, provider: "codex", access: "native" }),
    ).toBe(false);
  });

  it("enabled harness -> true", () => {
    // L3 A1：myagent 引擎可以当队长，与 provider 无关（provider 在 harness 语境下
    // 是「哪个 LLM 供应商」，如 deepseek/glm，不是「哪个 CLI」）。
    expect(
      hasLeadCapability({
        enabled: true,
        provider: "deepseek",
        access: "harness",
      }),
    ).toBe(true);
  });

  it("disabled harness -> false", () => {
    expect(
      hasLeadCapability({
        enabled: false,
        provider: "deepseek",
        access: "harness",
      }),
    ).toBe(false);
  });
});
