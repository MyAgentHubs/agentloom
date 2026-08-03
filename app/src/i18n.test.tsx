// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { I18nProvider, useI18n } from "./i18n";
import { OverviewHome } from "./components/OverviewHome";
import { SettingsLanguage } from "./components/settings/SettingsLanguage";

function Probe() {
  const { t } = useI18n();
  return <span>{t("overview.title")}</span>;
}

describe("i18n", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    window.localStorage.clear();
  });

  it("defaults to zh outside provider", () => {
    render(<Probe />);
    expect(screen.getByText("总览")).toBeInTheDocument();
  });

  it("renders English through provider and initializes the backend locale", async () => {
    render(
      <I18nProvider initialLocale="en">
        <OverviewHome sessions={[]} onOpen={() => {}} />
      </I18nProvider>,
    );
    expect(
      screen.getByRole("heading", { name: "Overview" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/No sessions yet/)).toBeInTheDocument();
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_ui_locale", {
        locale: "en",
      });
    });
  });

  it("settings language page switches the UI and backend locale", async () => {
    render(
      <I18nProvider initialLocale="zh">
        <SettingsLanguage />
        <Probe />
      </I18nProvider>,
    );
    expect(screen.getByText("语言与区域")).toBeInTheDocument();
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_ui_locale", {
        locale: "zh",
      });
    });
    fireEvent.click(screen.getByRole("radio", { name: /English English/ }));
    expect(screen.getByText("Language & Region")).toBeInTheDocument();
    expect(screen.getByText("Overview")).toBeInTheDocument();
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_ui_locale", {
        locale: "en",
      });
    });
  });

  it("keeps language initialization and switching functional when invoke throws", () => {
    invokeMock.mockImplementation(() => {
      throw new Error("Tauri unavailable");
    });

    expect(() => {
      render(
        <I18nProvider initialLocale="zh">
          <SettingsLanguage />
          <Probe />
        </I18nProvider>,
      );
    }).not.toThrow();
    fireEvent.click(screen.getByRole("radio", { name: /English English/ }));
    expect(screen.getByText("Language & Region")).toBeInTheDocument();
    expect(screen.getByText("Overview")).toBeInTheDocument();
  });

  it("keeps language initialization and switching functional when invoke rejects", async () => {
    invokeMock.mockRejectedValue(new Error("backend rejected locale"));

    render(
      <I18nProvider initialLocale="zh">
        <SettingsLanguage />
        <Probe />
      </I18nProvider>,
    );
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_ui_locale", {
        locale: "zh",
      });
    });
    fireEvent.click(screen.getByRole("radio", { name: /English English/ }));
    await waitFor(() => {
      expect(screen.getByText("Language & Region")).toBeInTheDocument();
      expect(screen.getByText("Overview")).toBeInTheDocument();
      expect(invokeMock).toHaveBeenCalledWith("set_ui_locale", {
        locale: "en",
      });
    });
  });
});

/**
 * i18n.tsx 里的 `messages` 对象没有导出（只导出了 `I18nKey` 类型，用于编译期约束），
 * 生产代码也不该为了测试专门开一个运行时导出口子。
 * 所以这里读源码文本、原样还原出 `const messages = { zh: {...}, en: {...} } as const`
 * 这段字面量并用 `Function` 求值，拿到真正的运行时对象来做 key 对齐检查——
 * 不修改 i18n.tsx。
 */
function loadI18nMessages(): {
  zh: Record<string, unknown>;
  en: Record<string, unknown>;
} {
  const source = readFileSync("src/i18n.tsx", "utf-8");
  const match = source.match(/const messages = (\{[\s\S]*?\n\} as const)/);
  if (!match) {
    throw new Error(
      "i18n.test.tsx: 未能在 i18n.tsx 中定位 `const messages = {...} as const` 字面量，" +
        "i18n.tsx 的结构可能变了，需要更新这条测试的解析逻辑。",
    );
  }
  const literalText = match[1].replace(/\s+as const$/, "");
  // eslint-disable-next-line no-new-func -- 从源码文本还原运行时对象，避免为测试改生产导出面
  const messages = new Function(`"use strict"; return (${literalText});`)() as {
    zh: Record<string, unknown>;
    en: Record<string, unknown>;
  };
  return messages;
}

/**
 * 递归收集 key 路径（形如 `settings.search.title`）。
 * messages.zh / messages.en 目前是纯扁平对象（key 本身就是点分路径字符串，
 * value 全是 string），这里仍写成递归版本以兼容万一之后改成真嵌套对象的情况：
 * - 遇到 plain object（非数组、非 null）→ 递归拼接前缀
 * - 遇到其他任何类型（string / function 插值 / number / 数组等）→ 当叶子收集
 */
function collectKeyPaths(obj: Record<string, unknown>, prefix = ""): string[] {
  const paths: string[] = [];
  for (const [key, value] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value !== null && typeof value === "object" && !Array.isArray(value)) {
      paths.push(...collectKeyPaths(value as Record<string, unknown>, path));
    } else {
      paths.push(path);
    }
  }
  return paths;
}

function formatKeyParityMismatch(
  missingInEn: string[],
  missingInZh: string[],
): string {
  const lines: string[] = ["zh / en 的 key 集合不一致："];
  if (missingInEn.length > 0) {
    lines.push(`  zh 有、en 缺（${missingInEn.length} 个）：`);
    for (const key of missingInEn) lines.push(`    - ${key}`);
  }
  if (missingInZh.length > 0) {
    lines.push(`  en 有、zh 缺（${missingInZh.length} 个）：`);
    for (const key of missingInZh) lines.push(`    - ${key}`);
  }
  return lines.join("\n");
}

describe("i18n key parity", () => {
  it("zh 和 en 的 key 集合完全一致（双向比较）", () => {
    const messages = loadI18nMessages();
    const zhPaths = collectKeyPaths(messages.zh).sort();
    const enPaths = collectKeyPaths(messages.en).sort();

    const zhSet = new Set(zhPaths);
    const enSet = new Set(enPaths);

    const missingInEn = zhPaths.filter((key) => !enSet.has(key));
    const missingInZh = enPaths.filter((key) => !zhSet.has(key));

    if (missingInEn.length > 0 || missingInZh.length > 0) {
      throw new Error(formatKeyParityMismatch(missingInEn, missingInZh));
    }

    expect(missingInEn).toEqual([]);
    expect(missingInZh).toEqual([]);
    expect(zhPaths.length).toBeGreaterThan(0);
  });
});
