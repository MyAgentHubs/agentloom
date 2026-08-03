import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { CSSProperties, FormEvent, ReactNode } from "react";
import { useI18n } from "../../i18n";
import type { I18nKey } from "../../i18n";

type SearchCategory = "ok" | "auth" | "rate_limit" | "network" | "missing_key";
type SearchTestResult = {
  ok: boolean;
  category: SearchCategory;
  raw_error?: string | null;
};
type TestState = "idle" | "testing" | "ok" | "err";
type SaveState = "idle" | "saving" | "saved" | "err";
type KeyStatus = "unknown" | "checking" | "configured" | "missing";
type SearchBackend = "duckduckgo" | "brave" | "exa";
type KeyedBackend = "brave" | "exa";
type UseThisState = "idle" | "switching" | "switched" | "err";

const BACKEND_META: Record<
  KeyedBackend,
  { apiName: string; label: string; registerUrl: string }
> = {
  brave: {
    apiName: "Brave Search API",
    label: "Brave",
    registerUrl: "https://brave.com/search/api",
  },
  exa: {
    apiName: "Exa Search API",
    label: "Exa",
    registerUrl: "https://exa.ai",
  },
};

const CATEGORY_KEY: Record<SearchCategory, I18nKey> = {
  ok: "settings.search.category.ok",
  auth: "settings.search.category.auth",
  rate_limit: "settings.search.category.rateLimit",
  network: "settings.search.category.network",
  missing_key: "settings.search.category.missingKey",
};

const styles = {
  root: {
    maxWidth: 760,
  },
  header: {
    alignItems: "flex-start",
    gap: 16,
    justifyContent: "space-between",
    marginBottom: 12,
  },
  headerCopy: {
    display: "flex",
    flexDirection: "column",
    gap: 2,
    minWidth: 0,
  },
  description: {
    color: "var(--ink-3)",
    fontSize: 11.5,
    lineHeight: 1.5,
    marginTop: 3,
    maxWidth: 640,
  },
  field: {
    display: "flex",
    flexDirection: "column",
    gap: 3,
    marginBottom: 10,
  },
  label: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    fontWeight: 600,
  },
  input: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink)",
    font: "inherit",
    fontSize: 12,
    padding: "6px 10px",
    width: "100%",
  },
  monoInput: {
    fontFamily: '"SF Mono", monospace',
    fontSize: 11,
  },
  hint: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    marginTop: 1,
  },
  statusGroup: {
    alignItems: "center",
    display: "inline-flex",
    flexShrink: 0,
    gap: 6,
  },
  status: {
    alignItems: "center",
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-2)",
    display: "inline-flex",
    fontSize: 11,
    gap: 6,
    padding: "5px 9px",
    whiteSpace: "nowrap",
  },
  checkBtn: {
    background: "transparent",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-2)",
    fontSize: 11,
    fontWeight: 600,
    padding: "4px 10px",
    cursor: "pointer",
    whiteSpace: "nowrap",
  },
  dot: {
    borderRadius: "50%",
    height: 7,
    width: 7,
  },
  dotReady: {
    background: "var(--green)",
    boxShadow: "0 0 0 2px rgba(106, 155, 92, 0.13)",
  },
  dotMissing: {
    background: "var(--ink-4)",
  },
  actions: {
    display: "flex",
    gap: 8,
    justifyContent: "flex-end",
    marginTop: 4,
  },
  error: {
    color: "var(--red)",
    fontSize: 11,
    textAlign: "right",
  },
  ddgRow: {
    alignItems: "center",
    display: "flex",
    gap: 10,
    justifyContent: "space-between",
  },
} satisfies Record<string, CSSProperties>;

export function SettingsSearch() {
  const { t } = useI18n();
  const [backend, setBackend] = useState<SearchBackend>("brave");
  const [apiKey, setApiKey] = useState("");
  // 挂载时不主动读 keychain（macOS 会弹钥匙串授权窗）；状态待用户点「检查」才探测。
  const [keyStatus, setKeyStatus] = useState<KeyStatus>("unknown");
  const [, setActiveBackendLoaded] = useState(false);
  const backendTouched = useRef(false);
  const [testState, setTestState] = useState<TestState>("idle");
  const [testResult, setTestResult] = useState<SearchTestResult | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [useThisState, setUseThisState] = useState<UseThisState>("idle");

  const needsKey = backend !== "duckduckgo";

  useEffect(() => {
    let cancelled = false;

    void invoke<string>("get_active_backend")
      .then((activeBackend) => {
        if (!cancelled && !backendTouched.current) {
          setBackend(normalizeBackend(activeBackend));
        }
      })
      .catch(() => {
        if (!cancelled && !backendTouched.current) {
          setBackend("brave");
        }
      })
      .finally(() => {
        if (!cancelled) {
          setActiveBackendLoaded(true);
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  function resetFeedback() {
    setTestState("idle");
    setTestResult(null);
    setSaveState("idle");
    setUseThisState("idle");
  }

  async function checkKeyState() {
    if (keyStatus === "checking") return;
    setKeyStatus("checking");
    try {
      const effectiveBackend = normalizeKeyedBackend(backend);
      const configured = await invoke<boolean>("get_search_key", {
        backend: effectiveBackend,
      });
      setKeyStatus(configured ? "configured" : "missing");
    } catch {
      setKeyStatus("missing");
    }
  }

  function changeBackend(nextBackend: SearchBackend) {
    setBackend(nextBackend);
    backendTouched.current = true;
    resetFeedback();
    // 切换 backend 不自动读 keychain；旧状态对新 backend 无意义，回到「未检查」等用户主动点检查。
    setKeyStatus("unknown");
  }

  async function runTest() {
    if (testState === "testing" || backend === "duckduckgo") return;
    setTestState("testing");
    setTestResult(null);
    setSaveState("idle");
    try {
      const result = await invoke<SearchTestResult>("test_search_service", {
        backend,
        apiKey: apiKey.trim(),
      });
      setTestResult(result);
      setTestState(result.ok ? "ok" : "err");
    } catch (error) {
      setTestResult({
        ok: false,
        category: "network",
        raw_error: String(error),
      });
      setTestState("err");
    }
  }

  async function useThisBackend() {
    if (useThisState === "switching" || backend !== "duckduckgo") return;
    setUseThisState("switching");
    try {
      await invoke("set_active_search_backend", { backend: "duckduckgo" });
      const activeBackend = normalizeBackend(
        await invoke<string>("get_active_backend"),
      );
      setBackend(activeBackend);
      setUseThisState(activeBackend === "duckduckgo" ? "switched" : "err");
    } catch {
      setUseThisState("err");
    }
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (saveState === "saving" || backend === "duckduckgo") return;
    setSaveState("saving");
    try {
      await invoke("set_search_key", { backend, key: apiKey.trim() });
      setBackend(normalizeBackend(await invoke<string>("get_active_backend")));
      setKeyStatus("configured");
      setSaveState("saved");
    } catch {
      setSaveState("err");
    }
  }

  const keyLabel =
    keyStatus === "unknown"
      ? t("settings.search.status.unknown")
      : keyStatus === "checking"
        ? t("settings.search.status.checking")
        : keyStatus === "configured"
          ? t("settings.search.status.configured")
          : t("settings.search.status.missing");

  const keyedMeta = needsKey ? BACKEND_META[backend as KeyedBackend] : null;

  return (
    <form
      aria-label={t("settings.search.formAriaLabel")}
      className="st-form"
      onSubmit={(event) => void save(event)}
      style={styles.root}
    >
      <div className="ob-disc-h" style={styles.header}>
        <div style={styles.headerCopy}>
          <span className="t">{t("settings.nav.search")}</span>
          <span style={styles.description}>{t("settings.search.intro")}</span>
        </div>
        {needsKey ? (
          <span style={styles.statusGroup}>
            <span style={styles.status}>
              <span
                style={{
                  ...styles.dot,
                  ...(keyStatus === "configured"
                    ? styles.dotReady
                    : styles.dotMissing),
                }}
              />
              {keyLabel}
            </span>
            <button
              type="button"
              style={styles.checkBtn}
              disabled={keyStatus === "checking"}
              onClick={() => void checkKeyState()}
            >
              {keyStatus === "checking"
                ? t("settings.search.status.checking")
                : t("settings.search.checkButton")}
            </button>
          </span>
        ) : null}
      </div>

      <div style={styles.field}>
        <span style={styles.label}>{t("settings.search.serviceLabel")}</span>
        <div
          role="radiogroup"
          aria-label={t("settings.search.serviceLabel")}
          className="st-search-seg"
        >
          <button
            type="button"
            role="radio"
            aria-checked={backend === "duckduckgo"}
            className={`st-search-option${backend === "duckduckgo" ? " active" : ""}`}
            onClick={() => changeBackend(normalizeBackend("duckduckgo"))}
          >
            DuckDuckGo
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={backend === "brave"}
            className={`st-search-option${backend === "brave" ? " active" : ""}`}
            onClick={() => changeBackend(normalizeBackend("brave"))}
          >
            Brave
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={backend === "exa"}
            className={`st-search-option${backend === "exa" ? " active" : ""}`}
            onClick={() => changeBackend(normalizeBackend("exa"))}
          >
            Exa
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={false}
            className="st-search-option"
            onClick={() => changeBackend(normalizeBackend("searxng"))}
            disabled
          >
            {t("settings.search.searxngComingSoon")}
          </button>
        </div>
      </div>

      {!needsKey ? (
        <>
          <div className="st-form-note plain" style={styles.ddgRow}>
            <span>{t("settings.search.ddgNote")}</span>
            <button
              type="button"
              className="ob-btn primary"
              disabled={useThisState === "switching"}
              onClick={() => void useThisBackend()}
            >
              {useThisState === "switching"
                ? t("settings.search.useThisSwitching")
                : t("settings.search.useThisButton")}
            </button>
          </div>
          {useThisState === "switched" ? (
            <div className="st-test-state ok" aria-live="polite">
              {t("settings.search.useThisSwitched")}
            </div>
          ) : null}
          {useThisState === "err" ? (
            <div style={styles.error} aria-live="polite">
              {t("settings.search.useThisError")}
            </div>
          ) : null}
        </>
      ) : (
        <>
          <Field label={t("settings.search.apiKeyLabel")}>
            <div className="st-test-row">
              <input
                id="search-api-key"
                aria-label={t("settings.search.apiKeyLabel")}
                autoComplete="off"
                placeholder={
                  keyStatus === "configured"
                    ? t("settings.search.placeholderConfigured")
                    : t("settings.search.placeholderEmpty", {
                        apiName: keyedMeta?.apiName ?? "",
                      })
                }
                style={{ ...styles.input, ...styles.monoInput }}
                type="password"
                value={apiKey}
                onChange={(event) => {
                  setApiKey(event.currentTarget.value);
                  resetFeedback();
                }}
              />
              <button
                type="button"
                className="st-test-btn"
                disabled={testState === "testing"}
                onClick={() => void runTest()}
              >
                {testState === "testing"
                  ? t("settings.search.testingButton")
                  : t("settings.search.testButton")}
              </button>
            </div>
          </Field>

          {testState !== "idle" ? (
            <div
              aria-live="polite"
              className={`st-test-state ${
                testState === "ok" ? "ok" : testState === "err" ? "err" : ""
              }`}
            >
              {testState === "testing"
                ? t("settings.search.testingButton")
                : null}
              {testState === "ok" ? t(CATEGORY_KEY.ok) : null}
              {testState === "err"
                ? t(CATEGORY_KEY[testResult?.category ?? "network"])
                : null}
            </div>
          ) : null}

          <div style={styles.actions}>
            <a
              href={keyedMeta?.registerUrl}
              onClick={(event) => {
                event.preventDefault();
                if (keyedMeta) void openUrl(keyedMeta.registerUrl);
              }}
              className="ob-link"
            >
              {t("settings.search.registerLink", {
                label: keyedMeta?.label ?? "",
              })}
            </a>
            <button
              type="submit"
              className="ob-btn primary"
              disabled={saveState === "saving"}
            >
              {saveState === "saving"
                ? t("settings.search.savingButton")
                : t("settings.search.saveButton")}
            </button>
          </div>
          <div className="st-form-note plain">
            {t("settings.search.saveNote")}
          </div>
          {saveState === "saved" ? (
            <div className="st-test-state ok" aria-live="polite">
              {t("settings.search.saved")}
            </div>
          ) : null}
          {saveState === "err" ? (
            <div style={styles.error} aria-live="polite">
              {t("settings.search.saveError")}
            </div>
          ) : null}
        </>
      )}
    </form>
  );
}

function normalizeBackend(value: string): SearchBackend {
  if (value === "exa") return "exa";
  if (value === "duckduckgo") return "duckduckgo";
  return "brave";
}

function normalizeKeyedBackend(value: string): KeyedBackend {
  return value === "exa" ? "exa" : "brave";
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label style={styles.field}>
      <span style={styles.label}>{label}</span>
      {children}
      {hint ? <span style={styles.hint}>{hint}</span> : null}
    </label>
  );
}
