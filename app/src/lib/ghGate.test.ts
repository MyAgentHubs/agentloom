import { describe, it, expect } from "vitest";
import { computeGate } from "./ghGate";

describe("computeGate", () => {
  it("工具尚未检测完成 → checking", () => {
    expect(
      computeGate(null, null, 0, { canBrew: false, installing: false }),
    ).toEqual({ kind: "checking" });
  });

  it("git 未装 → missingGit", () => {
    expect(
      computeGate(false, true, 0, { canBrew: false, installing: false }),
    ).toEqual({ kind: "missingGit" });
  });

  it("gh 未装 → missing 带 brew 能力", () => {
    expect(
      computeGate(true, false, 0, { canBrew: true, installing: false }),
    ).toEqual({
      kind: "missing",
      canBrewInstall: true,
      installing: false,
      installError: undefined,
    });
  });

  it("gh 装了无账户 → noAccount", () => {
    expect(
      computeGate(true, true, 0, { canBrew: true, installing: false }),
    ).toEqual({ kind: "noAccount" });
  });

  it("账户读取失败 → accountError", () => {
    expect(
      computeGate(
        true,
        true,
        0,
        { canBrew: false, installing: false },
        "TIMEOUT",
      ),
    ).toEqual({ kind: "accountError", message: "TIMEOUT" });
  });

  it("gh 装了有账户 → ready", () => {
    expect(
      computeGate(true, true, 2, { canBrew: false, installing: false }),
    ).toEqual({ kind: "ready" });
  });
});
