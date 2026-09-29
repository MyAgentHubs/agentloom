// qrPayloadErrorClassifier.test.ts — TDD 覆盖 qrPayloadErrorClassifier.ts。
//
// 覆盖表：
//   fragment_decode_failed              → "format"
//   json_parse_failed                   → "format"
//   shape_invalid（字段形态，无 "https"） → "format"
//   shape_invalid（"QR URL must use https"）                        → "origin_mismatch"
//   shape_invalid（"...https origin does not match...wss origin"）  → "origin_mismatch"
//
// 变异自证（任务书 §4 ④·本文件覆盖表里的一条）：手动把 classifyQrPayloadError 的判定条件改成
// `error.code === "shape_invalid"`（去掉 https 关键词判定，退化成"只要 shape_invalid 就是
// origin_mismatch"）——下面 "shape_invalid（字段形态）" 那条用例转红（期望 "format" 实收
// "origin_mismatch"）。改后跑 `npx vitest run src/ui/pairing/qrPayloadErrorClassifier.test.ts`
// 确认转红，再改回来复跑转绿；过程与结果记入 worker 报告 ⑤，代码已还原。

import { describe, expect, it } from "vitest";
import { QrPayloadError } from "../../pairing/qr-payload.ts";
import { classifyQrPayloadError } from "./qrPayloadErrorClassifier.ts";

describe("classifyQrPayloadError()", () => {
  it("classifies fragment_decode_failed as format", () => {
    const error = new QrPayloadError("fragment_decode_failed", "QR payload fragment is not valid base64url");
    expect(classifyQrPayloadError(error)).toBe("format");
  });

  it("classifies json_parse_failed as format", () => {
    const error = new QrPayloadError("json_parse_failed", "QR payload is not valid JSON");
    expect(classifyQrPayloadError(error)).toBe("format");
  });

  it("classifies plain shape_invalid (field shape, no https mention) as format", () => {
    const error = new QrPayloadError("shape_invalid", "QR payload room must be 32 lowercase hex chars");
    expect(classifyQrPayloadError(error)).toBe("format");
  });

  it('classifies "QR URL must use https" as origin_mismatch', () => {
    const error = new QrPayloadError("shape_invalid", "QR URL must use https");
    expect(classifyQrPayloadError(error)).toBe("origin_mismatch");
  });

  it('classifies "QR URL https origin does not match payload\'s wss origin" as origin_mismatch', () => {
    const error = new QrPayloadError(
      "shape_invalid",
      "QR URL https origin does not match payload's wss origin",
    );
    expect(classifyQrPayloadError(error)).toBe("origin_mismatch");
  });

  it("classifies malformed outer-URL-prefix shape_invalid (no https mention) as format", () => {
    const error = new QrPayloadError("shape_invalid", "QR URL prefix before #p= is not a valid URL");
    expect(classifyQrPayloadError(error)).toBe("format");
  });
});
