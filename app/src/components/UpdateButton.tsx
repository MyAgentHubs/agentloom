import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { renderBackendError } from "../lib/backendMsg";
import {
  check,
  downloadAndInstall,
  getUpdaterSnapshot,
  relaunch,
  reopen,
  swapBack,
  useUpdaterSnapshot,
} from "../lib/updaterStore";

const RING_R = 7;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_R;

/** `downloading` 的胶囊图标：已知总量画真实弧长，未知总量画不定环（CSS 转圈）。 */
function ProgressRing({ ratio }: { ratio: number | null }) {
  const dash =
    ratio === null ? RING_CIRCUMFERENCE * 0.28 : RING_CIRCUMFERENCE * ratio;
  return (
    <svg
      viewBox="0 0 18 18"
      width={15}
      height={15}
      aria-hidden
      className={
        ratio === null ? "updbtn__ring updbtn__ring--spin" : "updbtn__ring"
      }
    >
      <circle
        cx="9"
        cy="9"
        r={RING_R}
        fill="none"
        stroke="currentColor"
        strokeOpacity="0.25"
        strokeWidth="2"
      />
      <circle
        cx="9"
        cy="9"
        r={RING_R}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeDasharray={`${dash} ${RING_CIRCUMFERENCE - dash}`}
        transform="rotate(-90 9 9)"
      />
    </svg>
  );
}

const availableIcon = (
  <svg
    viewBox="0 0 24 24"
    width={15}
    height={15}
    fill="none"
    stroke="currentColor"
    strokeWidth={1.8}
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <path d="M12 4v11" />
    <path d="M7 11l5 5 5-5" />
    <path d="M5 19h14" />
  </svg>
);

const readyIcon = (
  <svg
    viewBox="0 0 24 24"
    width={15}
    height={15}
    fill="none"
    stroke="currentColor"
    strokeWidth={1.8}
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <path d="M4 12a8 8 0 0 1 14-5.3" />
    <path d="M20 12a8 8 0 0 1-14 5.3" />
    <path d="M18 3v4h-4" />
    <path d="M6 21v-4h4" />
  </svg>
);

/** `recovery_offered` 的胶囊图标——警示三角，配 `.updbtn--warn` 语义类。 */
const recoveryIcon = (
  <svg
    viewBox="0 0 24 24"
    width={15}
    height={15}
    fill="none"
    stroke="currentColor"
    strokeWidth={1.8}
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <path d="M12 3.5l9.5 16.5h-19L12 3.5z" />
    <path d="M12 9.5v4.2" />
    <path d="M12 16.8h.01" />
  </svg>
);

/**
 * 左侧栏 footer 更新胶囊：主路径点击即执行；下载中自身展示进度且不可点。
 * 只有 `recovery_offered` 保留一次轻量确认，避免误触换回旧版并重启。
 */
export function UpdateButton() {
  const { t } = useI18n();
  const snapshot = useUpdaterSnapshot();
  const state = snapshot.state;
  const buttonRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [recoveryOpen, setRecoveryOpen] = useState(false);
  const [position, setPosition] = useState<{
    top: number;
    left: number;
  } | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const visible =
    state.kind === "available" ||
    state.kind === "downloading" ||
    state.kind === "ready" ||
    state.kind === "recovery_offered" ||
    state.kind === "error";

  useEffect(() => {
    setActionError(null);
    if (state.kind !== "recovery_offered") setRecoveryOpen(false);
  }, [snapshot.revision, state.kind]);

  useEffect(() => {
    if (!recoveryOpen) return;
    const button = buttonRef.current;
    if (button) {
      const rect = button.getBoundingClientRect();
      const popoverWidth = popoverRef.current?.offsetWidth ?? 300;
      const popoverHeight = popoverRef.current?.offsetHeight ?? 0;
      setPosition({
        top: Math.max(8, rect.top - popoverHeight - 6),
        left: Math.min(
          Math.max(8, window.innerWidth - popoverWidth - 8),
          Math.max(8, rect.left),
        ),
      });
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setRecoveryOpen(false);
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (popoverRef.current?.contains(target)) return;
      if (buttonRef.current?.contains(target)) return;
      setRecoveryOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [recoveryOpen]);

  if (!visible) return null;

  const ratio =
    state.kind === "downloading" && state.total
      ? Math.min(1, state.downloaded / state.total)
      : null;
  const progressPct = ratio === null ? null : Math.round(ratio * 100);
  const stateError =
    state.kind === "error"
      ? renderBackendError(state.msg, t)
      : (state.kind === "ready" || state.kind === "recovery_offered") &&
          state.last_error
        ? renderBackendError(state.last_error, t)
        : null;
  const mainActionError =
    state.kind === "recovery_offered" ? null : actionError;
  const retryable =
    (state.kind === "ready" && Boolean(state.last_error)) ||
    Boolean(mainActionError);
  const buttonLabel = retryable
    ? t("updater.pill.failed")
    : state.kind === "available"
      ? t("updater.pill.available", { version: state.version })
      : state.kind === "downloading"
        ? progressPct === null
          ? t("updater.pill.downloading.indeterminate")
          : t("updater.pill.downloading.percent", { pct: progressPct })
        : state.kind === "ready"
          ? t("updater.pill.ready")
          : t("updater.pill.failed");

  const runDownload = () => {
    setActionError(null);
    downloadAndInstall().catch((err: unknown) => {
      setActionError(renderBackendError(err, t));
    });
  };
  const retryAfterError = () => {
    setActionError(null);
    void check(true)
      .then(() => {
        if (getUpdaterSnapshot().state.kind === "available") runDownload();
      })
      .catch((err: unknown) => {
        setActionError(renderBackendError(err, t));
      });
  };
  const handleClick = () => {
    if (state.kind === "downloading") return;
    if (state.kind === "recovery_offered") {
      setRecoveryOpen((open) => !open);
      return;
    }
    if (state.kind === "error") {
      if (state.retry === "reopen") {
        setActionError(null);
        reopen().catch((err: unknown) => {
          setActionError(renderBackendError(err, t));
        });
      } else {
        retryAfterError();
      }
      return;
    }
    if (retryable && state.kind === "ready") {
      setActionError(null);
      relaunch().catch((err: unknown) => {
        setActionError(renderBackendError(err, t));
      });
      return;
    }
    if (state.kind === "available" || retryable) {
      runDownload();
      return;
    }
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

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className={
          state.kind === "error"
            ? "updbtn updbtn--error"
            : state.kind === "recovery_offered"
              ? "updbtn updbtn--warn"
              : "updbtn"
        }
        aria-label={buttonLabel}
        aria-busy={state.kind === "downloading"}
        title={mainActionError ?? stateError ?? buttonLabel}
        disabled={state.kind === "downloading"}
        onClick={handleClick}
      >
        {state.kind === "downloading" ? (
          <ProgressRing ratio={ratio} />
        ) : state.kind === "ready" ? (
          readyIcon
        ) : state.kind === "recovery_offered" || state.kind === "error" ? (
          recoveryIcon
        ) : (
          availableIcon
        )}
        <span className="updbtn__label">{buttonLabel}</span>
      </button>
      {recoveryOpen &&
        state.kind === "recovery_offered" &&
        createPortal(
          <div
            ref={popoverRef}
            role="dialog"
            aria-label={t("updater.popover.label")}
            className="updpop"
            style={{
              top: position?.top ?? 0,
              left: position?.left ?? 0,
            }}
          >
            <div className="updpop__title">{t("updater.recovery.title")}</div>
            <div className="updpop__body">
              {t("updater.recovery.body", {
                version: state.target_version,
              })}
            </div>
            {stateError && <div className="updpop__error">{stateError}</div>}
            <div className="updpop__actions">
              <button
                type="button"
                className="updpop__primary"
                onClick={handleSwapBack}
              >
                {t("updater.recovery.action")}
              </button>
            </div>
            {actionError && <div className="updpop__error">{actionError}</div>}
          </div>,
          document.body,
        )}
    </>
  );
}
