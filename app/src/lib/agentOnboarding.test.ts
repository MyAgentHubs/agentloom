import { describe, expect, it } from "vitest";
import { shouldShowInstallGuide } from "./agentOnboarding";

const readyWithoutAgents = {
  agentsReady: true,
  runtimeDetectResolved: true,
  availableAgentsCount: 0,
  dismissed: false,
};

describe("shouldShowInstallGuide", () => {
  it("agents 尚未加载完成时不显示", () => {
    expect(
      shouldShowInstallGuide({ ...readyWithoutAgents, agentsReady: false }),
    ).toBe(false);
  });

  it("runtime 检测尚未返回或失败时不显示", () => {
    expect(
      shouldShowInstallGuide({
        ...readyWithoutAgents,
        runtimeDetectResolved: false,
      }),
    ).toBe(false);
  });

  it("存在可用 agent 时不显示", () => {
    expect(
      shouldShowInstallGuide({
        ...readyWithoutAgents,
        availableAgentsCount: 1,
      }),
    ).toBe(false);
  });

  it("本次启动已关闭时不显示", () => {
    expect(
      shouldShowInstallGuide({ ...readyWithoutAgents, dismissed: true }),
    ).toBe(false);
  });

  it("加载和检测完成、零可用 agent 且未关闭时显示", () => {
    expect(shouldShowInstallGuide(readyWithoutAgents)).toBe(true);
  });
});
