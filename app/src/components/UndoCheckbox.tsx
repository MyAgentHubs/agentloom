type Props = {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onChange: (checked: boolean) => void;
};

export function UndoCheckbox({
  checked,
  disabled = false,
  label,
  onChange,
}: Props) {
  return (
    <label className={`check-wrap${disabled ? " disabled" : ""}`} title={label}>
      <input
        className="undo-check"
        type="checkbox"
        checked={checked}
        disabled={disabled}
        aria-label={label}
        onChange={(event) => onChange(event.currentTarget.checked)}
      />
    </label>
  );
}
