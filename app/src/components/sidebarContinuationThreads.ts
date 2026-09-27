import type { Session } from "../types/agent";

export function continuationChildrenByParent(
  list: Session[],
): Map<string, Session[]> {
  const childrenByParent = new Map<string, Session[]>();
  for (const session of list) {
    if (!session.parent_session_id) continue;
    const children = childrenByParent.get(session.parent_session_id) ?? [];
    children.push(session);
    childrenByParent.set(session.parent_session_id, children);
  }
  return childrenByParent;
}

export function arrangeContinuationThreads(list: Session[]): Session[] {
  const byId = new Map(list.map((s) => [s.id, s]));
  const childrenByParent = continuationChildrenByParent(list);
  const emitted = new Set<string>();
  const arranged: Session[] = [];

  function emitThread(root: Session) {
    let current: Session | undefined = root;
    while (current && !emitted.has(current.id)) {
      arranged.push(current);
      emitted.add(current.id);
      const pointedChild: Session | undefined = current.continued_to_session_id
        ? byId.get(current.continued_to_session_id)
        : undefined;
      const fallbackChildren: Session[] =
        childrenByParent.get(current.id) ?? [];
      current =
        pointedChild ??
        (fallbackChildren.length === 1 ? fallbackChildren[0] : undefined);
    }
  }

  for (const session of list) {
    if (emitted.has(session.id)) continue;
    if (session.parent_session_id && byId.has(session.parent_session_id))
      continue;
    emitThread(session);
  }
  for (const session of list) {
    if (!emitted.has(session.id)) emitThread(session);
  }
  return arranged;
}
