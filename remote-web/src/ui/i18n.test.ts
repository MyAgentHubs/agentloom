// These tests cover the PAIRING_MESSAGES table to protect translation key consistency.
//
// zh/en 两张表若 key 集合不一致，只会在真被用到那个缺 key 的 locale 时才会暴露（`t()` 对不
// 存在的 key 落回 zh 兜底/key 本身，见 i18n.ts 文件头注——那条兜底路径本身是故意保留的运行时
// 容错，不代表"两表长期不同步"这件事不该在测试里被主动测出来）。
//
// 照 app 侧 `src/i18n.test.tsx`（`describe("i18n key parity", ...)`）的既有写法：不改
// i18n.ts 的导出面（`PAIRING_MESSAGES` 本就不导出，避免专为测试拉出一个生产不需要的导出），
// 读源码文本原样还原出 `const PAIRING_MESSAGES = {...} as const` 字面量并用 `Function` 求值
// 拿到真正的运行时对象。本文件是 `.test.ts`（非 `.tsx`），落 vitest.config.ts 的 "logic"
// node 环境项目，不是 "ui" jsdom 项目——用相对路径 `readFileSync` 没有 fixtures.ts 头注里记
// 的那个 jsdom `URL` 类替换坑。

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * `PAIRING_MESSAGES` 目前是纯扁平对象（key 本身就是点分路径字符串，如
 * `"pairing.manualEntry.heading"`），不需要 app 侧 `collectKeyPaths` 那种递归收集——直接
 * `Object.keys()` 即可。
 */
function loadPairingMessages(): {
  zh: Record<string, unknown>;
  en: Record<string, unknown>;
} {
  const source = readFileSync("src/ui/i18n.ts", "utf8");
  const match = source.match(
    /const PAIRING_MESSAGES = (\{[\s\S]*?\n\} as const)/,
  );
  if (!match) {
    throw new Error(
      "i18n.test.ts: 未能在 i18n.ts 中定位 `const PAIRING_MESSAGES = {...} as const` 字面量，" +
        "i18n.ts 的结构可能变了，需要更新这条测试的解析逻辑。",
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

describe("PAIRING_MESSAGES i18n key parity", () => {
  it("zh 和 en 的 key 集合完全一致（双向比较）", () => {
    const messages = loadPairingMessages();
    const zhKeys = Object.keys(messages.zh).sort();
    const enKeys = Object.keys(messages.en).sort();

    const zhSet = new Set(zhKeys);
    const enSet = new Set(enKeys);

    const missingInEn = zhKeys.filter((key) => !enSet.has(key));
    const missingInZh = enKeys.filter((key) => !zhSet.has(key));

    if (missingInEn.length > 0 || missingInZh.length > 0) {
      throw new Error(formatKeyParityMismatch(missingInEn, missingInZh));
    }

    expect(missingInEn).toEqual([]);
    expect(missingInZh).toEqual([]);
    expect(zhKeys.length).toBeGreaterThan(0);
  });
});
