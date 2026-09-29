import type { ReactNode } from "react";
import type { ConnectionSessionPhase } from "../connection/types.ts";
import { SessionStreamScreen, type HistoryLoadError } from "../ui/stream/SessionStreamScreen.tsx";
import { Composer } from "../ui/composer/Composer.tsx";
import { ConnectionBanner, type DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import type { deriveSelectedSessionView } from "./appRuntimeSelectedSessionView.ts";

type SelectedSessionView = ReturnType<typeof deriveSelectedSessionView>;

interface AppRuntimeSessionViewProps {
  connectionState: {
    phase: ConnectionSessionPhase;
    phaseChangedAtMs: number;
    disconnectedSinceMs: number | null;
  };
  desktopPresence: DesktopPresence;
  streamProps: SelectedSessionView["streamProps"];
  onBack: () => void;
  historyLoading: boolean;
  historyError: HistoryLoadError | null;
  onLoadEarlier: () => void;
  onDecisionChoose: (decisionId: string, option: string) => void;
  decisionAnswerOverrides: SelectedSessionView["decisionAnswerOverrides"];
  onStop: () => void;
  stopBadge: SelectedSessionView["stopBadge"];
  msgFetchStates: SelectedSessionView["msgFetchStates"];
  onLoadFullText: SelectedSessionView["onLoadFullText"];
  verboseEnabled: boolean;
  onSend: (text: string) => void;
  sendBadge: SelectedSessionView["sendBadge"];
  debugPanel: ReactNode;
}

export function AppRuntimeSessionView({
  connectionState,
  desktopPresence,
  streamProps,
  onBack,
  historyLoading,
  historyError,
  onLoadEarlier,
  onDecisionChoose,
  decisionAnswerOverrides,
  onStop,
  stopBadge,
  msgFetchStates,
  onLoadFullText,
  verboseEnabled,
  onSend,
  sendBadge,
  debugPanel,
}: AppRuntimeSessionViewProps) {
  return (
    <div className="app-runtime-stream">
      <ConnectionBanner
        phase={connectionState.phase}
        phaseChangedAtMs={connectionState.disconnectedSinceMs ?? connectionState.phaseChangedAtMs}
        desktopPresence={desktopPresence}
      />
      <SessionStreamScreen
        {...streamProps}
        onBack={onBack}
        historyLoading={historyLoading}
        historyError={historyError}
        onLoadEarlier={onLoadEarlier}
        onDecisionChoose={onDecisionChoose}
        decisionAnswerOverrides={decisionAnswerOverrides}
        onStop={onStop}
        stopBadge={stopBadge}
        msgFetchStates={msgFetchStates}
        onLoadFullText={onLoadFullText}
        verboseEnabled={verboseEnabled}
      />
      <Composer
        onSend={onSend}
        sendBadge={sendBadge}
      />
      {debugPanel}
    </div>
  );
}
