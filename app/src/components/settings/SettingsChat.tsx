import { useI18n, type I18nKey } from "../../i18n";
import { useChatVerbosity, type ChatVerbosity } from "../../lib/chatVerbosity";

const OPTIONS: {
  value: ChatVerbosity;
  titleKey: I18nKey;
  descKey: I18nKey;
}[] = [
  {
    value: "full",
    titleKey: "settings.chat.full.title",
    descKey: "settings.chat.full.desc",
  },
  {
    value: "summary",
    titleKey: "settings.chat.summary.title",
    descKey: "settings.chat.summary.desc",
  },
  {
    value: "minimal",
    titleKey: "settings.chat.minimal.title",
    descKey: "settings.chat.minimal.desc",
  },
];

/**
 * Settings「对话」分区（spec §2C·决策点 4）：桌面 chat 过程细节显示级别三选一。
 * 只读写 `useChatVerbosity`——本机偏好，不跨设备同步（spec §4 明确不做）。
 */
export function SettingsChat() {
  const { t } = useI18n();
  const [verbosity, setVerbosity] = useChatVerbosity();

  return (
    <div className="st-lang">
      <div className="st-lang__head">
        <h2>{t("settings.chat.title")}</h2>
        <p>{t("settings.chat.subtitle")}</p>
      </div>
      <div className="st-lang__field">
        <div className="st-lang__label">{t("settings.chat.groupLabel")}</div>
        <div
          role="radiogroup"
          aria-label={t("settings.chat.groupLabel")}
          className="st-chat__group"
        >
          {OPTIONS.map((option) => {
            const active = verbosity === option.value;
            const descId = `chat-verbosity-desc-${option.value}`;
            return (
              <label
                key={option.value}
                className={`st-chat__option${active ? " st-chat__option--active" : ""}`}
              >
                <input
                  type="radio"
                  name="chat-verbosity"
                  value={option.value}
                  checked={active}
                  onChange={() => setVerbosity(option.value)}
                  aria-label={t(option.titleKey)}
                  aria-describedby={descId}
                  className="st-chat__radio"
                />
                <span className="st-chat__text">
                  <span className="st-chat__title">{t(option.titleKey)}</span>
                  <span id={descId} className="st-chat__desc">
                    {t(option.descKey)}
                  </span>
                </span>
              </label>
            );
          })}
        </div>
      </div>
    </div>
  );
}
