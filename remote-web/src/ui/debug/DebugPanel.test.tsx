import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { FrameDiagnosticsSnapshot } from "../../app/appRuntimeCore.ts";
import {
  DebugPanel,
  createConnectionDiagnostics,
  recordConnectionLog,
} from "./DebugPanel.tsx";

const diagnostics: FrameDiagnosticsSnapshot = {
  framesSeen: 12,
  plaintextControl: 1,
  kindSkipped: 2,
  decryptFailed: 3,
  parseFailed: 4,
  lastParseFailedReason: "malformed_fields",
  routingRejected: 5,
  missingIds: 6,
  storeError: 7,
  storeDuplicate: 8,
  applied: 9,
  milestoneSessionNull: 10,
  snapshotRejected: 11,
  defensiveDefault: 12,
  liveSessionNull: 13,
  liveUnknownDelta: 14,
  liveDroppedNoRun: 15,
  liveWatermarkRejected: 16,
  lastDroppedFrameT: "text_delta",
  lastDroppedKind: "live",
  lastDroppedErrorMessage: null,
};

afterEach(() => {
  cleanup();
  window.history.replaceState({}, "", "/");
});

describe("DebugPanel", () => {
  it("?debug=1 时渲染单列诊断计数、最后 parse reason 与最后丢弃帧", () => {
    window.history.replaceState({}, "", "/sessions?debug=0&debug=1");
    render(<DebugPanel diagnostics={diagnostics} connectionDiagnostics={createConnectionDiagnostics()} />);

    expect(screen.getByTestId("debug-panel")).toBeTruthy();
    expect(screen.getByTestId("debug-framesSeen").textContent).toBe("12");
    expect(screen.getByTestId("debug-routingRejected").textContent).toBe("5");
    expect(screen.getByTestId("debug-storeError").textContent).toBe("7");
    expect(screen.getByTestId("debug-snapshotRejected").textContent).toBe("11");
    expect(screen.getByTestId("debug-defensiveDefault").textContent).toBe("12");
    expect(screen.getByTestId("debug-liveSessionNull").textContent).toBe("13");
    expect(screen.getByTestId("debug-liveUnknownDelta").textContent).toBe("14");
    expect(screen.getByTestId("debug-liveWatermarkRejected").textContent).toBe("16");
    expect(screen.getByTestId("debug-parseReason").textContent).toBe("malformed_fields");
    expect(screen.getByTestId("debug-lastDrop").textContent).toBe("text_delta / live");
  });

  it("没有 debug=1 时零渲染", () => {
    window.history.replaceState({}, "", "/sessions#p=pairing-payload");
    const { container } = render(
      <DebugPanel diagnostics={diagnostics} connectionDiagnostics={createConnectionDiagnostics()} />,
    );
    expect(container.childElementCount).toBe(0);
  });

  it("注入连接日志后显示最近 WS 关闭码/reason、分类结果与带时间的事件尾巴", () => {
    window.history.replaceState({}, "", "/?debug=1");
    let connectionDiagnostics = createConnectionDiagnostics();
    connectionDiagnostics = recordConnectionLog(
      connectionDiagnostics,
      "info",
      "connection closed",
      { code: 4401, reason: "token_reauthorization_failed" },
      new Date(2026, 7, 15, 12, 34, 56).getTime(),
    );
    connectionDiagnostics = recordConnectionLog(
      connectionDiagnostics,
      "info",
      "connection outcome classified",
      { classification: "needs_refresh" },
      new Date(2026, 7, 15, 12, 34, 57).getTime(),
    );

    render(<DebugPanel diagnostics={diagnostics} connectionDiagnostics={connectionDiagnostics} />);

    expect(screen.getByTestId("debug-lastWsClose").textContent).toBe(
      "4401 / token_reauthorization_failed",
    );
    expect(screen.getByTestId("debug-lastConnectionResult").textContent).toBe("needs_refresh");
    const events = screen.getAllByTestId("debug-connectionEvent");
    expect(events).toHaveLength(2);
    expect(events[0].textContent).toBe("12:34:56 connection closed");
    expect(events[1].textContent).toBe("12:34:57 connection outcome classified");
  });

  it("连接事件缓冲只保留最近 20 条且不保留未列入白名单的 context", () => {
    let connectionDiagnostics = createConnectionDiagnostics();
    for (let index = 0; index < 25; index += 1) {
      connectionDiagnostics = recordConnectionLog(
        connectionDiagnostics,
        "info",
        `event-${index}`,
        { token: "secret-token", payload: { secret: true } },
        index * 1_000,
      );
    }

    expect(connectionDiagnostics.events).toHaveLength(20);
    expect(connectionDiagnostics.events[0]?.name).toBe("event-5");
    expect(connectionDiagnostics.events[19]?.name).toBe("event-24");
    expect(JSON.stringify(connectionDiagnostics)).not.toContain("secret-token");
    expect(JSON.stringify(connectionDiagnostics)).not.toContain("payload");
  });

  it("没有较新的分类结果时显示最近一次错误事件名而不暴露错误 context", () => {
    window.history.replaceState({}, "", "/?debug=1");
    const connectionDiagnostics = recordConnectionLog(
      createConnectionDiagnostics(),
      "error",
      "connection lifecycle failed unexpectedly",
      { error: "Bearer secret-token", payload: "private-frame" },
      0,
    );

    render(<DebugPanel diagnostics={diagnostics} connectionDiagnostics={connectionDiagnostics} />);

    expect(screen.getByTestId("debug-lastConnectionResult").textContent).toBe(
      "connection lifecycle failed unexpectedly",
    );
    expect(screen.getByTestId("debug-panel").textContent).not.toContain("secret-token");
    expect(screen.getByTestId("debug-panel").textContent).not.toContain("private-frame");
  });

  it("关闭 reason 即使含有 hex token 也会在进入面板前再次脱敏", () => {
    window.history.replaceState({}, "", "/?debug=1");
    const token = "a".repeat(64);
    const connectionDiagnostics = recordConnectionLog(
      createConnectionDiagnostics(),
      "info",
      "connection closed",
      { code: 4001, reason: `expired token=${token}` },
      0,
    );

    render(<DebugPanel diagnostics={diagnostics} connectionDiagnostics={connectionDiagnostics} />);

    expect(screen.getByTestId("debug-lastWsClose").textContent).toBe("4001 / expired token=***");
    expect(screen.getByTestId("debug-panel").textContent).not.toContain(token);
  });
});
