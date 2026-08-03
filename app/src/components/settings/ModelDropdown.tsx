import { useDropdown } from "../../hooks/useDropdown";
import { CUSTOM_MODEL_SENTINEL } from "./agentFormHelpers";
import { useI18n } from "../../i18n";

type Props = {
  value: string;
  options: string[];
  liveModels: string[];
  onChange: (model: string) => void;
  onSelectCustom: () => void;
  disabled?: boolean;
  defaultOption?: { value: string; label: string };
  placeholder?: string;
};

export function ModelDropdown({
  value,
  options,
  liveModels,
  onChange,
  onSelectCustom,
  disabled,
  defaultOption,
  placeholder,
}: Props) {
  const { t } = useI18n();
  const dd = useDropdown();
  const liveSet = new Set(liveModels);
  const effectivePlaceholder =
    placeholder ?? t("settings.modelDropdown.placeholder");
  return (
    <div className="dd" ref={dd.containerRef}>
      <button
        type="button"
        className="st-model-trigger"
        {...dd.triggerProps}
        disabled={disabled}
        onClick={dd.toggle}
      >
        {value === defaultOption?.value
          ? defaultOption.label
          : value || effectivePlaceholder}{" "}
        ▾
      </button>
      {dd.open && (
        <div className="dd__menu" role="menu">
          {defaultOption ? (
            <button
              type="button"
              role="menuitemradio"
              aria-checked={value === defaultOption.value}
              className={`dd__item${value === defaultOption.value ? " dd__item--on" : ""}`}
              onClick={() => {
                onChange(defaultOption.value);
                dd.close();
              }}
            >
              {defaultOption.label}
            </button>
          ) : null}
          {options.map((opt) =>
            opt === CUSTOM_MODEL_SENTINEL ? (
              <button
                key={opt}
                type="button"
                role="menuitemradio"
                aria-checked={false}
                className="dd__item st-model-custom"
                onClick={() => {
                  onSelectCustom();
                  dd.close();
                }}
              >
                {t("settings.modelDropdown.custom")}
              </button>
            ) : (
              <button
                key={opt}
                type="button"
                role="menuitemradio"
                aria-checked={opt === value}
                className={`dd__item${opt === value ? " dd__item--on" : ""}`}
                onClick={() => {
                  onChange(opt);
                  dd.close();
                }}
              >
                {opt}
                {liveSet.has(opt) ? (
                  <span className="st-model-live">
                    {t("settings.modelDropdown.live")}
                  </span>
                ) : null}
              </button>
            ),
          )}
        </div>
      )}
    </div>
  );
}
