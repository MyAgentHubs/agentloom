export type InstallGuideVisibilityInput = {
  agentsReady: boolean;
  runtimeDetectResolved: boolean;
  availableAgentsCount: number;
  dismissed: boolean;
};

export function shouldShowInstallGuide({
  agentsReady,
  runtimeDetectResolved,
  availableAgentsCount,
  dismissed,
}: InstallGuideVisibilityInput): boolean {
  return (
    agentsReady &&
    runtimeDetectResolved &&
    availableAgentsCount === 0 &&
    !dismissed
  );
}
