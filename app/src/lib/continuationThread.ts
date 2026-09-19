import type { Session } from "../types/agent";

export function sessionHasContinuationThread(
  session: Session,
  childrenByParent: Map<string, Session[]>,
): boolean {
  return (
    session.parent_session_id !== null ||
    session.continued_to_session_id !== null ||
    (childrenByParent.get(session.id)?.length ?? 0) > 0
  );
}
