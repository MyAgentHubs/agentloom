import type { Session } from "../types/agent";

/** session-hover-menu §5.4：测试夹具 factory。新字段给默认值，避免每处手填。 */
export function makeSession(partial: Partial<Session> = {}): Session {
  return {
    id: "s-test",
    title: "测试会话",
    repo_id: "local-default",
    namespace_id: "local",
    in_place: false,
    group_id: null,
    parent_session_id: null,
    continued_to_session_id: null,
    created_at: 0,
    pinned: false,
    unread: false,
    archived: false,
    archived_at: null,
    total_input_tokens: 0,
    total_output_tokens: 0,
    ...partial,
  };
}
