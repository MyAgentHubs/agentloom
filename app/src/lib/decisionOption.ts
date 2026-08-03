/**
 * 决策打扰收敛刀 T1·症状 A 排版对齐：把一句决策选项文案切成「短标签 + 说明」两段
 * （原型基准 mockups/04-agent-collab/agent-team-decision-inflow.html 行 660-676：
 * `<b>标签</b><span>说明</span>` 两段式，标签粗体在前、说明灰字在后）。
 *
 * 切分规则：在**首个**出现的分隔符处切（"，" / "：" / "。" / " — " / " - "，取字符串里
 * 位置最靠前的那个，不按固定优先级），且仅当：
 *   - 标签段（切点前）长度 ≤ 20 字符
 *   - 说明段（切点后）非空（trim 后非空串）
 * 两条都满足才真的切；任一条不满足（含压根没有分隔符）就整句当标签、不产生说明段——
 * 对齐 DecisionCard 既有契约「无分隔符的选项不渲二级说明」。
 */
const SPLIT_MARKERS = ["，", "：", "。", " — ", " - "];
const MAX_LABEL_LENGTH = 20;

export type SplitDecisionOption = {
  label: string;
  desc: string | null;
};

export function splitDecisionOption(option: string): SplitDecisionOption {
  let cutIndex = -1;
  let markerLength = 0;

  for (const marker of SPLIT_MARKERS) {
    const idx = option.indexOf(marker);
    if (idx === -1) continue;
    if (cutIndex === -1 || idx < cutIndex) {
      cutIndex = idx;
      markerLength = marker.length;
    }
  }

  if (cutIndex === -1) {
    return { label: option, desc: null };
  }

  const label = option.slice(0, cutIndex);
  const rest = option.slice(cutIndex + markerLength);

  if (label.length > MAX_LABEL_LENGTH || rest.trim() === "") {
    return { label: option, desc: null };
  }

  return { label, desc: rest };
}
