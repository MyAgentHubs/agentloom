use super::{
    agent_event, dispatch_card_from_output, epoch_millis, is_hidden_orchestration_tool, lookup,
    AgentEvent, Block, BlockCardKind, BlockToolStatus, DisplayReducer, ToolStatus, TOOL_OUTPUT_CAP,
};

impl DisplayReducer {
    pub(super) fn feed_tool_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolStarted {
                id,
                tool,
                summary,
                card,
            } => {
                if is_hidden_orchestration_tool(tool) {
                    self.hidden_tool_ids.push(id.clone());
                    if tool == "mcp__agentloom__dispatch_worker" {
                        self.dispatch_tool_ids.push(id.clone());
                        self.dispatch_started_at.push((id.clone(), epoch_millis()));
                    }
                    return;
                }
                self.blocks.push(Block::Tool {
                    id: id.clone(),
                    tool: tool.clone(),
                    summary: summary.clone(),
                    card: match card {
                        agent_event::CardKind::Command => BlockCardKind::Command,
                        agent_event::CardKind::Compact => BlockCardKind::Compact,
                    },
                    status: BlockToolStatus::Running,
                    exit_code: None,
                    output: None,
                });
                self.tool_index.push((id.clone(), self.blocks.len() - 1));
            }
            AgentEvent::ToolOutputDelta { id, text } => {
                if self.hidden_tool_ids.iter().any(|hidden_id| hidden_id == id) {
                    return;
                }
                match self.tool_output.iter_mut().find(|(key, _)| key == id) {
                    Some((_, accumulated)) => accumulated.push_str(text),
                    None => self.tool_output.push((id.clone(), text.clone())),
                }
            }
            AgentEvent::ToolCompleted {
                id,
                status,
                exit_code,
                output,
            } => {
                if let Some(position) = self.hidden_tool_ids.iter().position(|item| item == id) {
                    self.hidden_tool_ids.remove(position);
                    if let Some(dispatch_position) = self
                        .dispatch_tool_ids
                        .iter()
                        .position(|dispatch_id| dispatch_id == id)
                    {
                        self.dispatch_tool_ids.remove(dispatch_position);
                        let started_at = self
                            .dispatch_started_at
                            .iter()
                            .position(|(dispatch_id, _)| dispatch_id == id)
                            .map(|started_position| {
                                self.dispatch_started_at.remove(started_position).1
                            })
                            .unwrap_or_else(epoch_millis);
                        if let Some(card) = dispatch_card_from_output(output.as_deref(), started_at)
                        {
                            self.blocks.push(card);
                        }
                    }
                    return;
                }
                if let Some(index) = lookup(&self.tool_index, id) {
                    let accumulated = self
                        .tool_output
                        .iter()
                        .find(|(key, _)| key == id)
                        .map(|(_, value)| value.as_str())
                        .unwrap_or("");
                    let merged = output
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .unwrap_or(accumulated);
                    let excerpt = if merged.is_empty() {
                        None
                    } else {
                        Some(agent_event::truncate_output(merged, TOOL_OUTPUT_CAP))
                    };
                    if let Block::Tool {
                        status: block_status,
                        exit_code: block_exit_code,
                        output: block_output,
                        ..
                    } = &mut self.blocks[index]
                    {
                        *block_status = match status {
                            ToolStatus::Ok => BlockToolStatus::Ok,
                            ToolStatus::Failed => BlockToolStatus::Failed,
                        };
                        *block_exit_code = *exit_code;
                        *block_output = excerpt;
                    }
                }
            }
            _ => unreachable!("feed_tool_event called with a non-tool event"),
        }
    }

    pub(super) fn feed_approval_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ApprovalRequested {
                approval_id,
                run_id,
                tool,
                command,
                summary,
                cwd,
                request_kind,
                ..
            } => {
                self.blocks.push(Block::Approval {
                    approval_id: approval_id.clone(),
                    run_id: run_id.clone(),
                    tool: tool.clone(),
                    command: command.clone(),
                    summary: summary.clone(),
                    cwd: cwd.clone(),
                    request_kind: request_kind.clone(),
                    status: "pending".to_string(),
                });
                self.approval_index
                    .push((approval_id.clone(), self.blocks.len() - 1));
            }
            AgentEvent::ApprovalResolved {
                approval_id,
                decision,
                ..
            } => {
                if let Some(index) = lookup(&self.approval_index, approval_id) {
                    if let Block::Approval { status, .. } = &mut self.blocks[index] {
                        if status == "pending" {
                            *status = if decision == "approved" {
                                "approved".to_string()
                            } else {
                                "rejected".to_string()
                            };
                        }
                    }
                }
            }
            _ => unreachable!("feed_approval_event called with a non-approval event"),
        }
    }

    pub(super) fn feed_run_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::NeedsDecision { changes, .. } => {
                self.blocks.push(Block::ScopeChange {
                    changes: changes.clone(),
                });
            }
            AgentEvent::Error { message } => {
                self.last_error = Some(message.clone());
            }
            AgentEvent::Blocked { message, .. } => {
                self.last_blocked = Some(message.clone());
            }
            AgentEvent::Completed { .. } => {
                self.saw_completed = true;
            }
            AgentEvent::ContextCompacted { .. } => {
                self.blocks.push(Block::ContextCompacted {});
            }
            AgentEvent::HeadTruncated {} => {
                self.blocks.push(Block::ContextTruncated {});
            }
            _ => unreachable!("feed_run_event called with an unrelated event"),
        }
    }
}
