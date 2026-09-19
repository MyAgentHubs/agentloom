import { useState } from "react";
import { useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";
import { relativeTime } from "../../lib/relativeTime";
import {
  check,
  discardUpdate,
  downloadAndInstall,
  relaunch,
  reopen,
  skipVersion,
  swapBack,
  useUpdaterSnapshot,
} from "../../lib/updaterStore";
import { useMarkdown } from "../../lib/useMarkdown";
import type { TranslationKey } from "../../i18n";

const appVersion =
  typeof __APP_VERSION__ === "undefined" ? "dev" : __APP_VERSION__;

/**
 * 关于页更新区（`SettingsAbout.tsx` / `AboutDialog.tsx` 共用）：当前版本 +
 * 上次检查时间 + 手动检查按钮；`available` 展示完整更新说明与跳过入口；
 * `disabled` 按 reason 显示对应文案并隐藏按钮；`error` 走既有 `AL_ERR`
 * 信封解析器渲染完整可读错误
 * （设计 §2D「前端」·§2F 验收 5/8/9）。
 */
export function UpdateSection() {
  const { t } = useI18n();
  const MarkdownBody = useMarkdown();
  const snapshot = useUpdaterSnapshot();
  const state = snapshot.state;
  // relaunch()/discardUpdate()（Ready）与 swapBack()（RecoveryOffered）互斥
  // 渲染，共用一个错误态展示位。
  const [actionError, setActionError] = useState<string | null>(null);
  const stateError =
    state.kind === "ready" && state.last_error
      ? renderBackendError(state.last_error, t)
      : state.kind === "error"
        ? renderBackendError(state.msg, t)
        : state.kind === "recovery_offered" && state.last_error
          ? renderBackendError(state.last_error, t)
          : null;

  const handleCheck = () => {
    if (state.kind === "error" && state.retry === "reopen") {
      setActionError(null);
      reopen().catch((err: unknown) => {
        setActionError(renderBackendError(err, t));
      });
    } else {
      void check(true);
    }
  };
  const handleDownload = () => {
    void downloadAndInstall();
  };
  const handleSkip = () => {
    if (state.kind === "available") void skipVersion(state.version);
  };
  const handleRelaunch = () => {
    setActionError(null);
    relaunch().catch((err: unknown) => {
      setActionError(renderBackendError(err, t));
    });
  };
  const handleSwapBack = () => {
    setActionError(null);
    swapBack().catch((err: unknown) => {
      setActionError(renderBackendError(err, t));
    });
  };
  const handleDiscard = () => {
    setActionError(null);
    discardUpdate().catch((err: unknown) => {
      setActionError(renderBackendError(err, t));
    });
  };

  if (state.kind === "disabled") {
    const reasonKey: TranslationKey =
      state.reason === "dev"
        ? "updater.about.disabled.dev"
        : state.reason === "platform"
          ? "updater.about.disabled.platform"
          : "updater.about.disabled.unsigned";
    return (
      <div className="updsec">
        <div className="updsec__title">{t("updater.about.title")}</div>
        <div className="updsec__row updsec__row--disabled">{t(reasonKey)}</div>
      </div>
    );
  }

  // recovery_offered：启动期发现「自身正运行在暂存路径」（用户因新版起不来
  // 手动打开了旧版）——单独一支渲染，不进下面 parts 拼接管线（布局与其余态
  // 明显不同：没有「上次检查」概念，只有一个换回动作）。
  if (state.kind === "recovery_offered") {
    return (
      <div className="updsec">
        <div className="updsec__title">{t("updater.about.title")}</div>
        <div className="updsec__row updsec__row--recovery">
          <span>{t("updater.recovery.title")}</span>
        </div>
        <div className="updsec__row updsec__row--recovery">
          <span>
            {t("updater.recovery.body", { version: state.target_version })}
          </span>
        </div>
        {stateError && <div className="updsec__error">{stateError}</div>}
        <div className="updsec__actions">
          <button
            type="button"
            className="updpop__primary"
            onClick={handleSwapBack}
          >
            {t("updater.recovery.action")}
          </button>
        </div>
        {actionError && actionError !== stateError && (
          <div className="updsec__error">{actionError}</div>
        )}
      </div>
    );
  }

  const parts: string[] = [
    t("updater.about.currentVersion", { version: appVersion }),
  ];
  if (state.kind === "up_to_date" || state.kind === "error") {
    const r = relativeTime(state.checked_at, Date.now());
    parts.push(
      t("updater.about.lastChecked", {
        when: t(r.key as TranslationKey, { n: r.n }),
      }),
    );
  } else if (state.kind === "idle") {
    parts.push(t("updater.about.neverChecked"));
  }
  if (state.kind === "up_to_date") parts.push(t("updater.about.upToDate"));
  if (state.kind === "available")
    parts.push(t("updater.about.availableVersion", { version: state.version }));
  if (state.kind === "downloading") parts.push(t("updater.about.downloading"));
  if (state.kind === "staging") parts.push(t("updater.about.staging"));
  if (state.kind === "ready")
    parts.push(t("updater.about.readyVersion", { version: state.version }));
  if (state.kind === "swapping") parts.push(t("updater.about.swapping"));

  return (
    <div className="updsec">
      <div className="updsec__title">{t("updater.about.title")}</div>
      <div className="updsec__row">
        <span>{parts.join(" · ")}</span>
        {state.kind === "checking" ? (
          <button type="button" className="updsec__check" disabled>
            <span className="updsec__spin" aria-hidden />
            {t("updater.about.checking")}
          </button>
        ) : state.kind === "idle" ||
          state.kind === "up_to_date" ||
          state.kind === "error" ||
          state.kind === "ready" ? (
          <button type="button" className="updsec__check" onClick={handleCheck}>
            {state.kind === "error" && state.retry === "reopen"
              ? t("updater.about.reopenButton")
              : t("updater.about.checkButton")}
          </button>
        ) : null}
      </div>
      {state.kind === "available" && (
        <>
          {state.notes && (
            <div className="updsec__notes">
              <div className="updsec__notes-title">
                {t("updater.popover.notes")}
              </div>
              <div className="updsec__notes-body">
                {MarkdownBody ? (
                  <MarkdownBody streaming={false}>{state.notes}</MarkdownBody>
                ) : (
                  <div style={{ whiteSpace: "pre-wrap" }}>{state.notes}</div>
                )}
              </div>
            </div>
          )}
          <div className="updsec__actions">
            <button
              type="button"
              className="updpop__primary"
              onClick={handleDownload}
            >
              {t("updater.action.download")}
            </button>
            <button
              type="button"
              className="updpop__secondary"
              onClick={handleSkip}
            >
              {t("updater.action.skip")}
            </button>
          </div>
        </>
      )}
      {state.kind === "ready" && stateError && (
        <>
          <div className="updsec__error">{stateError}</div>
          <div className="updsec__error">{t("updater.ready.retryHint")}</div>
        </>
      )}
      {state.kind === "ready" && (
        <div className="updsec__actions">
          <button
            type="button"
            className="updpop__primary"
            onClick={handleRelaunch}
          >
            {t("updater.action.relaunch")}
          </button>
          <button
            type="button"
            className="updpop__secondary"
            onClick={handleDiscard}
          >
            {t("updater.ready.discard")}
          </button>
        </div>
      )}
      {actionError && actionError !== stateError && (
        <div className="updsec__error">{actionError}</div>
      )}
      {state.kind === "error" && stateError && (
        <div className="updsec__error">{stateError}</div>
      )}
    </div>
  );
}
