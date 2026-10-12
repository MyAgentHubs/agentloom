export type AppView = "overview" | "session" | "intro";
export type AppRoute = {
  view: AppView;
  sessionId: string | null;
  namespaceId: string | null;
  repoId: string | null;
};

export function appRouteKey(route: AppRoute): string {
  if (route.view === "session") return `session:${route.sessionId ?? ""}`;
  return `${route.view}:${route.namespaceId ?? ""}:${route.repoId ?? ""}`;
}

export function routeFromState(
  view: AppView,
  currentId: string | null,
  namespaceId: string,
  repoId: string | null,
): AppRoute {
  return {
    view,
    sessionId: view === "session" ? currentId : null,
    namespaceId: view === "session" ? null : namespaceId,
    repoId: view === "session" ? null : repoId,
  };
}
