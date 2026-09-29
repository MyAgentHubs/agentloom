// PairingErrorView.tsx — T6f1 · 错误/失效态（任务书 §2：「错误/失效态：needs_repair（明文提示
// 重新扫码·不清凭据）」）。两种 kind 共用一个组件、不同文案分支：
//   - "qr-error"：畸形/来源不符的扫码或手输 payload（区分 format / origin_mismatch，见
//     qrPayloadErrorClassifier.ts）。
//   - "needs-repair"：PairingSession 收到 device_revoked，转 needs_repair 态——只提示，**不**在
//     这里调 keyStore.clear()（M0 §9.6 硬约束：relay 的明文提示不可信，凭据不自动清）。

import { useI18n } from "../i18n.ts";

export type PairingErrorViewProps =
  | { kind: "qr-error"; category: "format" | "origin_mismatch"; message: string }
  | { kind: "needs-repair"; reason: string | null };

export function PairingErrorView(props: PairingErrorViewProps) {
  const { t } = useI18n();

  if (props.kind === "qr-error") {
    return (
      <div
        className="pairing-error"
        data-testid="pairing-state-error"
        data-error-kind="qr-error"
        data-category={props.category}
      >
        <h1 className="pairing-heading">
          {props.category === "origin_mismatch" ? t("pairing.error.qr.originHeading") : t("pairing.error.qr.formatHeading")}
        </h1>
        <p className="pairing-hint">
          {props.category === "origin_mismatch" ? t("pairing.error.qr.originHint") : t("pairing.error.qr.formatHint")}
        </p>
      </div>
    );
  }

  return (
    <div className="pairing-error" data-testid="pairing-state-error" data-error-kind="needs-repair">
      <h1 className="pairing-heading">{t("pairing.error.needsRepair.heading")}</h1>
      <p className="pairing-hint">
        {props.reason
          ? t("pairing.error.needsRepair.hintWithReason", { reason: props.reason })
          : t("pairing.error.needsRepair.hint")}
      </p>
    </div>
  );
}
