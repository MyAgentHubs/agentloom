import type { Block } from "../types/agent";

export function blockWeight(block: Block): number {
  if (block.type === "text" || block.type === "thinking") {
    return block.text.length;
  }
  if (block.type === "tool") {
    return block.id.length + block.status.length + (block.output?.length ?? 0);
  }
  if (block.type === "gate_card" || block.type === "draft_failed") {
    return block.type.length;
  }
  if (block.type === "team_run") {
    return (
      block.run_id.length +
      (block.lead?.length ?? 0) +
      block.members.reduce(
        (sum, member) =>
          sum +
          member.assignment_id.length +
          member.name.length +
          member.status.length +
          member.steps_done +
          member.steps_total +
          member.blocks.length,
        0,
      )
    );
  }
  if (block.type === "coding_task") {
    return (
      block.run_id.length +
      block.assignment_id.length +
      block.worker_name.length +
      block.phase.length +
      (block.detail?.length ?? 0) +
      (block.verify_cmd?.length ?? 0)
    );
  }
  if (block.type === "lead_summary") {
    return (
      block.run_id.length +
      block.summary_source.length +
      block.status.kind.length +
      block.sections.reduce(
        (sum, section) =>
          sum + section.heading.length + (section.body_richtext?.length ?? 0),
        0,
      ) +
      block.findings.length +
      block.artifact_refs.length
    );
  }
  if (block.type === "dispatch_card") {
    return (
      block.run_id.length +
      block.member.assignment_id.length +
      block.member.status.length +
      block.member.steps_done +
      block.member.steps_total +
      block.member.blocks.length
    );
  }
  return block.type.length;
}
