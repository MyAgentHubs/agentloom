// qrPayloadErrorClassifier.ts — T6f1 · 把 QrPayloadError 分成 UI 要的两类文案桶（任务书 §2：
// 「畸形 payload → 错误态（区分「格式错/origin 不符」文案）」）。
//
// qr-payload.ts（只读引用，不改）目前用同一个 code "shape_invalid" 覆盖两类语义不同的错误：
// ① 字段形态本身不对（room/pairing_token/desktop_pub 格式、v 不是 1、不是 JSON 对象……）；
// ② 外层 https origin 与 payload 里 relay_url 派生的 wss origin 不一致（或外层 URL 干脆没用
//    https）——这是 §3 第 1 条防线套餐里的安全校验，不是简单的"格式错"。
// qr-payload.ts 没有为 origin 不符单开 error code，只能按 message 内容分流：origin 校验失败的
// 两条错误信息都固定包含 "https"（"QR URL must use https" / "QR URL https origin does not match
// payload's wss origin"）；其余 shape_invalid 分支（包括"外层 URL 本身不合法"这条——那是 URL 解析
// 失败，不是"origin 不符合法但不匹配"）都不含这个词。这是弱启发式，但两条真实错误消息文本已经在
// qr-payload.ts 里钉死（改动会被 qr-payload.test.ts 挡住），不容易漂移。

import { QrPayloadError } from "../../pairing/qr-payload.ts";

export type QrPayloadErrorCategory = "format" | "origin_mismatch";

export function classifyQrPayloadError(error: QrPayloadError): QrPayloadErrorCategory {
  if (error.code === "shape_invalid" && /https/i.test(error.message)) {
    return "origin_mismatch";
  }
  return "format";
}
