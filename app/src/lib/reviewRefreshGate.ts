import type { RightPanelTab } from "../components/RightPanelTabs";

export function isReviewPanelVisible(
  view: string,
  rightPanelOpen: boolean,
  rightPanelTab: RightPanelTab | null,
): boolean {
  return view === "session" && rightPanelOpen && rightPanelTab === "review";
}

export function shouldFetchOnSwitch(
  panelVisible: boolean,
  hasCache: boolean,
): boolean {
  return !panelVisible && !hasCache;
}
