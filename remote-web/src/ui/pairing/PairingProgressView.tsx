// PairingProgressView.tsx — T6f1 · 配对进行态（任务书 §2：「UI 只对状态机的 phase 渲染：
// awaiting_accept/awaiting_ready/activated/needs_repair」）。activated/needs_repair 两个终态直接
// 委托给共享的 PairedPlaceholder / PairingErrorView（跟"冷启动直接读到已有凭据"复用同一份 UI，
// 不重复写一遍文案）。

import type { PairingPhase } from "../../pairing/pairing-session.ts";
import { useI18n } from "../i18n.ts";
import { PairedPlaceholder } from "./PairedPlaceholder.tsx";
import { PairingErrorView } from "./PairingErrorView.tsx";

export function PairingProgressView({
  phase,
  deviceId,
  revocationReason,
}: {
  phase: PairingPhase;
  deviceId: string | null;
  revocationReason: string | null;
}) {
  const { t } = useI18n();

  if (phase === "activated") {
    return <PairedPlaceholder deviceId={deviceId} />;
  }
  if (phase === "needs_repair") {
    return <PairingErrorView kind="needs-repair" reason={revocationReason} />;
  }

  const label =
    phase === "awaiting_accept"
      ? t("pairing.progress.awaitingAccept")
      : phase === "awaiting_ready"
        ? t("pairing.progress.awaitingReady")
        : t("pairing.progress.idle");

  return (
    <div className="pairing-progress" data-testid="pairing-state-progress" data-phase={phase}>
      <div className="pairing-progress__spinner" aria-hidden="true" />
      <p className="pairing-progress__label">{label}</p>
    </div>
  );
}
