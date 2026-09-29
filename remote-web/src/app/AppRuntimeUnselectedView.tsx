import type { ReactNode } from "react";
import type { ConnectionSessionPhase } from "../connection/types.ts";
import type { SessionIndexRow } from "../events/parseFrame.ts";
import { SessionListScreen } from "../ui/sessions/SessionListScreen.tsx";
import { SettingsScreen } from "../ui/settings/SettingsScreen.tsx";
import { ConnectionBanner, type DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import type { AppRuntimeCore } from "./appRuntimeCore.ts";

interface AppRuntimeUnselectedViewProps {
  showSettings: boolean;
  connectionState: {
    phase: ConnectionSessionPhase;
    phaseChangedAtMs: number;
    disconnectedSinceMs: number | null;
  };
  desktopPresence: DesktopPresence;
  verboseEnabled: boolean;
  onToggleVerbose: (next: boolean) => void;
  cacheEnabled: boolean;
  onToggleCache: (next: boolean) => void;
  onUnpair?: () => void;
  onNeedsRepair: () => void;
  onSettingsBack: () => void;
  sessions: SessionIndexRow[];
  activeRepo: AppRuntimeCore["indexProjection"]["activeRepo"];
  onSelect: (sessionId: string) => void;
  onOpenSettings: () => void;
  debugPanel: ReactNode;
}

export function AppRuntimeUnselectedView({
  showSettings,
  connectionState,
  desktopPresence,
  verboseEnabled,
  onToggleVerbose,
  cacheEnabled,
  onToggleCache,
  onUnpair,
  onNeedsRepair,
  onSettingsBack,
  sessions,
  activeRepo,
  onSelect,
  onOpenSettings,
  debugPanel,
}: AppRuntimeUnselectedViewProps) {
  // The settings screen is reachable only from the session list with no session selected (a
  // low-frequency entry point, like project switching). Entering it does not affect
  // `selectedSessionId` or connection state; `showSettings` is purely local UI state.
  if (showSettings) {
    return (
      <>
        <ConnectionBanner
          phase={connectionState.phase}
          phaseChangedAtMs={connectionState.disconnectedSinceMs ?? connectionState.phaseChangedAtMs}
          desktopPresence={desktopPresence}
        />
        <SettingsScreen
          verboseEnabled={verboseEnabled}
          onToggleVerbose={onToggleVerbose}
          cacheEnabled={cacheEnabled}
          onToggleCache={onToggleCache}
          // Explicit unpairing no longer unconditionally reuses `onNeedsRepair`.
          // The connection may still be alive and writing frames when this path is triggered,
          // unlike authentication-related needs_repair, when `ConnectionSession` is already
          // tearing it down. `RootRouter.tsx` now supplies a dedicated `onUnpair` for this path
          // (unmount this component to stop the connection, then delete the database); only
          // when it is omitted do we fall back to `onNeedsRepair` for existing callers and tests.
          onUnpair={onUnpair ?? onNeedsRepair}
          onBack={onSettingsBack}
        />
        {debugPanel}
      </>
    );
  }
  return (
    <>
      <ConnectionBanner
        phase={connectionState.phase}
        phaseChangedAtMs={connectionState.disconnectedSinceMs ?? connectionState.phaseChangedAtMs}
        desktopPresence={desktopPresence}
      />
      <SessionListScreen
        sessions={sessions}
        selectedId={null}
        onSelect={onSelect}
        activeRepoName={activeRepo?.name ?? null}
        onOpenSettings={onOpenSettings}
      />
      {debugPanel}
    </>
  );
}
