// PairedPlaceholder.tsx — T6f1 · 已配对态占位页（任务书 §2：「已配对态：占位页（「已连接·会话
// 列表施工中」·读 key-store 有凭据即显示）」）。两条路径都渲染这同一个组件：① 冷启动时
// PairingScreen 直接从 keyStore 读到既有凭据；② 本次会话里 PairingSession 走到 activated。

import { useI18n } from "../i18n.ts";

export function PairedPlaceholder({ deviceId }: { deviceId: string | null }) {
  const { t } = useI18n();
  return (
    <div className="pairing-paired" data-testid="pairing-state-paired">
      <h1 className="pairing-heading">{t("pairing.paired.heading")}</h1>
      <p className="pairing-hint">{t("pairing.paired.hint")}</p>
      {deviceId && (
        <p className="pairing-device-id" data-testid="pairing-paired-device-id">
          {t("pairing.paired.deviceId", { deviceId })}
        </p>
      )}
    </div>
  );
}
