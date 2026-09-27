/**
 * Split a decision option's text into a short label and an explanation so that
 * the label appears in bold, followed by the explanation in grey.
 * This preserves the compact two-part presentation without forcing long text into a label.
 *
 * Split at the first occurring separator among "，" / "：" / "。" / " — " / " - ":
 * use the earliest position in the text, not a fixed separator priority, and only when:
 *   - The label segment before the separator is at most 20 characters long.
 *   - The explanation segment after the separator is nonempty after trimming.
 * Both conditions must hold; otherwise (including no separator), keep the entire text as the label.
 * This preserves DecisionCard's contract: options without a separator have no secondary explanation.
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
