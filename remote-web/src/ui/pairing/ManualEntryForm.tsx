// ManualEntryForm.tsx — T6f1 · 引导态里"无 fragment"时的手输/粘贴兜底（任务书 §2：「无 fragment
// → 手输/粘贴 payload 兜底输入框（§8 P0-a1「裸 JSON 粘贴兜底」的 Web 侧）」）。直接吃
// `parseQrPayload` 支持的裸 JSON / 裸 fragment 值两种形状（qr-payload.ts 本身已经兼容，这里不用
// 另外分支）。

import { useState, type FormEvent } from "react";
import { useI18n } from "../i18n.ts";
import type { QrPayloadErrorCategory } from "./qrPayloadErrorClassifier.ts";

export interface ManualEntryError {
  category: QrPayloadErrorCategory;
  message: string;
}

export function ManualEntryForm({
  onSubmit,
  error,
}: {
  onSubmit: (raw: string) => void;
  error: ManualEntryError | null;
}) {
  const { t } = useI18n();
  const [value, setValue] = useState("");

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    onSubmit(value);
  }

  return (
    <div className="pairing-manual-entry" data-testid="pairing-state-manual-entry">
      <h1 className="pairing-heading">{t("pairing.manualEntry.heading")}</h1>
      <p className="pairing-hint">{t("pairing.manualEntry.hint")}</p>
      <form onSubmit={handleSubmit}>
        <textarea
          data-testid="pairing-manual-entry-textarea"
          value={value}
          onChange={(event) => setValue(event.target.value)}
          rows={6}
          placeholder='{"v":1,"relay_url":"wss://...","room":"...","pairing_token":"...","desktop_pub":"..."}'
        />
        <button type="submit" data-testid="pairing-manual-entry-submit">
          {t("pairing.manualEntry.submit")}
        </button>
      </form>
      {error && (
        <p className="pairing-error-text" data-testid="pairing-manual-entry-error" data-category={error.category}>
          {error.category === "origin_mismatch"
            ? t("pairing.manualEntry.error.originMismatch")
            : t("pairing.manualEntry.error.format")}
        </p>
      )}
    </div>
  );
}
