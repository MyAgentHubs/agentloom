import { useEffect, useRef, useState } from "react";
import type {
  GhGate,
  RepoListView,
  RepoManagePanelProps,
} from "../types/repoManage";
import { useI18n } from "../i18n";
import { RepoList } from "./RepoList";

type Props =
  | (Omit<RepoManagePanelProps, "listState"> & {
      view: RepoListView;
      listState?: never;
    })
  | RepoManagePanelProps;

function legacyListStateToView(
  listState: RepoManagePanelProps["listState"],
): RepoListView {
  if (listState.kind === "loading") return { kind: "cold-loading" };
  if (listState.kind === "ready") {
    return { kind: "data", repos: listState.repos, refreshing: false };
  }
  if (listState.kind === "offline") {
    return { kind: "cold-error", message: "OFFLINE" };
  }
  return { kind: "cold-error", message: listState.message };
}

function listErrorText(
  message: string,
  login: string,
  t: ReturnType<typeof useI18n>["t"],
): string {
  if (message === "TIMEOUT" || message.includes("TIMEOUT")) {
    return t("repoManage.error.timeout");
  }
  if (message === "OFFLINE" || message.includes("OFFLINE")) {
    return t("repoManage.error.offline");
  }
  if (message.startsWith("NO_TOKEN")) {
    const tokenLogin = message.split(":")[1] || login;
    return t("repoManage.error.authExpired", { login: tokenLogin });
  }
  return message || t("repoManage.error.loadFailed");
}

function connectErrorText(
  code: string,
  t: ReturnType<typeof useI18n>["t"],
): string {
  if (code === "NOT_GIT") return t("repoConnection.error.notGit");
  if (code === "NOT_GITHUB") return t("repoConnection.error.notGithub");
  if (code === "NO_COMMITS") return t("repoConnection.error.noCommits");
  if (code.startsWith("ALREADY_ADDED"))
    return t("repoConnection.error.alreadyAdded");
  return t("repoConnection.error.generic");
}

function Gate({
  gate,
  onInstallGh,
  onRefreshAccounts,
  onRetryTools,
}: {
  gate: Exclude<GhGate, { kind: "ready" }>;
  onInstallGh: () => void;
  onRefreshAccounts: () => void;
  onRetryTools: () => void;
}) {
  const { t } = useI18n();
  if (gate.kind === "checking") {
    return (
      <div className="ob-gate-card">
        <div className="nm">{t("repoManage.gate.checking.title")}</div>
        <div className="sub">{t("repoManage.gate.checking.description")}</div>
      </div>
    );
  }
  if (gate.kind === "missingGit") {
    return (
      <div className="ob-gate-card">
        <div className="nm">{t("repoManage.gate.missingGit.title")}</div>
        <div className="sub">{t("repoManage.gate.missingGit.description")}</div>
        <a
          className="ob-link"
          href="https://git-scm.com/downloads"
          target="_blank"
          rel="noreferrer"
        >
          {t("repoManage.gate.missingGit.install")}
        </a>
        <button className="ob-btn" onClick={onRetryTools}>
          {t("repoManage.recheck")}
        </button>
      </div>
    );
  }
  if (gate.kind === "missing") {
    return (
      <div className="ob-gate-card">
        <div className="nm">{t("repoManage.gate.missing.title")}</div>
        <div className="sub">{t("repoManage.gate.missing.description")}</div>
        {gate.canBrewInstall ? (
          <button
            className="ob-btn primary"
            disabled={gate.installing}
            onClick={onInstallGh}
          >
            {gate.installing
              ? t("repoManage.gate.installing")
              : t("repoManage.gate.install")}
          </button>
        ) : (
          <a
            className="ob-link"
            href="https://cli.github.com/"
            target="_blank"
            rel="noreferrer"
          >
            {t("repoManage.gate.manualInstall")}
          </a>
        )}
        {gate.installError && (
          <div className="ob-gate-err">{gate.installError}</div>
        )}
        <button className="ob-btn" onClick={onRetryTools}>
          {t("repoManage.recheck")}
        </button>
      </div>
    );
  }
  if (gate.kind === "accountError") {
    const message =
      gate.message === "TIMEOUT" || gate.message.includes("TIMEOUT")
        ? t("repoManage.error.accountTimeout")
        : listErrorText(gate.message, "", t);
    return (
      <div className="ob-gate-card">
        <div className="nm">{t("repoManage.gate.accountError.title")}</div>
        <div className="sub">{message}</div>
        <button className="ob-btn" onClick={onRefreshAccounts}>
          {t("repoManage.retry")}
        </button>
      </div>
    );
  }
  return (
    <div className="ob-gate-card">
      <div className="nm">{t("repoManage.gate.connect.title")}</div>
      <div className="sub">
        {t("repoManage.gate.connect.instructions.prefix")}{" "}
        <code>gh auth login</code>
        {t("repoManage.gate.connect.instructions.suffix")}
      </div>
      <button className="ob-btn" onClick={onRefreshAccounts}>
        {t("repoManage.refresh")}
      </button>
    </div>
  );
}

function GithubIcon() {
  return (
    <span className="gh">
      <svg viewBox="0 0 16 16">
        <path d="M8 0a8 8 0 00-2.5 15.6c.4.07.55-.17.55-.38v-1.5c-2 .37-2.5-.5-2.7-.95-.1-.23-.5-.94-.8-1.13-.3-.15-.7-.52 0-.53.6 0 1.05.58 1.2.82.72 1.2 1.87.87 2.33.66.07-.52.28-.87.5-1.07-1.77-.2-3.64-.89-3.64-3.95 0-.87.3-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82a7.6 7.6 0 014 0c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.5.56.82 1.28.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48v2.2c0 .2.15.46.55.38A8 8 0 008 0z" />
      </svg>
    </span>
  );
}

export function RepoManagePanel(p: Props) {
  const { t } = useI18n();
  const [connectHintOpen, setConnectHintOpen] = useState(false);
  const [acctMenuOpen, setAcctMenuOpen] = useState(false);
  const acctRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!acctMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (!acctRef.current?.contains(e.target as Node)) setAcctMenuOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [acctMenuOpen]);
  const selCount = p.selected.size;
  const view =
    "view" in p && p.view
      ? p.view
      : legacyListStateToView((p as RepoManagePanelProps).listState);
  const repoCounts =
    view.kind === "data"
      ? {
          cloned: view.repos.filter((repo) => repo.cloned).length,
          remote: view.repos.filter((repo) => !repo.cloned).length,
        }
      : null;

  return (
    <>
      <div className="rm-head">
        <div className="sc-h">{t("repoManage.title")}</div>
        <div className="sc-sub">{t("repoManage.subtitle")}</div>
        <div className="rm-bar" ref={acctRef}>
          <button
            type="button"
            className="acct-dd"
            aria-label={t("repoManage.switchAccount")}
            aria-haspopup="menu"
            aria-expanded={acctMenuOpen}
            disabled={p.accounts.length === 0}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => setAcctMenuOpen((v) => !v)}
          >
            <GithubIcon />
            <b>@{p.selectedLogin || "—"}</b>
            <span className="chev">▾</span>
          </button>
          {p.gate.kind === "checking" ? (
            <span
              className="rm-account-status"
              role="status"
              aria-label={t("repoManage.checkingAria")}
            >
              <span className="rm-account-status__spinner" aria-hidden="true" />
              {t("repoManage.checking")}
            </span>
          ) : p.gate.kind === "missingGit" ? (
            <span className="rm-account-status">
              {t("repoManage.status.missingGit")}
            </span>
          ) : p.gate.kind === "missing" ? (
            <span className="rm-account-status">
              {t("repoManage.status.missingGh")}
            </span>
          ) : p.gate.kind === "accountError" ? (
            <span className="rm-account-status">
              {t("repoManage.status.accountError")}
            </span>
          ) : p.gate.kind === "noAccount" ? (
            <span className="rm-account-status">
              {t("repoManage.status.noAccount")}
            </span>
          ) : view.kind === "cold-loading" ? (
            <span
              className="rm-account-status"
              role="status"
              aria-label={t("repoManage.loadingAria")}
            >
              <span className="rm-account-status__spinner" aria-hidden="true" />
              {t("repoManage.loading")}
            </span>
          ) : repoCounts ? (
            <span className="rm-account-status">
              {t("repoManage.counts", repoCounts)}
            </span>
          ) : null}
          {acctMenuOpen && (
            <div className="acct-menu" role="menu">
              {p.accounts.map((a) => (
                <button
                  key={a.login}
                  type="button"
                  role="menuitemradio"
                  aria-checked={a.login === p.selectedLogin}
                  className={`acct-menu-item${a.login === p.selectedLogin ? " active" : ""}`}
                  onClick={() => {
                    p.onSelectAccount(a.login);
                    setAcctMenuOpen(false);
                  }}
                >
                  <GithubIcon />@{a.login}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
      {p.gate.kind !== "ready" ? (
        <div className="rm-state">
          <Gate
            gate={p.gate}
            onInstallGh={p.onInstallGh}
            onRefreshAccounts={p.onRefreshAccounts}
            onRetryTools={p.onRetryList}
          />
        </div>
      ) : view.kind === "idle" ? (
        <div className="rm-state">
          <div className="ob-offline">
            {t("repoManage.idle")}
            <button className="ob-btn" onClick={p.onRetryList}>
              {t("repoManage.read")}
            </button>
          </div>
        </div>
      ) : view.kind === "cold-loading" ? (
        <div className="rm-state">
          <div className="ob-disc-h">
            <div className="ob-sk w3" />
          </div>
          <div className="ob-sk line" />
          {Array.from({ length: 6 }, (_, i) => (
            <div className="ob-repo" key={i}>
              <span className="rico">
                <span className="ob-sk" style={{ width: 14, height: 14 }} />
              </span>
              <div className="body">
                <div className="ob-sk w2" />
                <div className="ob-sk w1" />
              </div>
              <div className="right">
                <div className="ob-sk" style={{ width: 54 }} />
              </div>
            </div>
          ))}
        </div>
      ) : view.kind === "cold-error" ? (
        <div className="rm-state">
          <div className="ob-offline">
            {listErrorText(view.message, p.selectedLogin, t)}
            <button className="ob-btn" onClick={p.onRetryList}>
              {t("repoManage.retry")}
            </button>
          </div>
        </div>
      ) : (
        <RepoList
          repos={view.repos}
          selectedLogin={p.selectedLogin}
          search={p.search}
          onSearchChange={p.onSearchChange}
          filter={p.filter}
          onFilterChange={p.onFilterChange}
          selected={p.selected}
          onToggleSelect={p.onToggleSelect}
          cloneProgress={p.cloneProgress}
          onRetry={p.onRetry}
          onOpenSession={p.onOpenSession}
        />
      )}
      <div className="rm-foot">
        {p.gate.kind === "ready" && selCount > 0 ? (
          <div className="ob-batchbar">
            <div className="summary">
              <span>
                {t("repoManage.selection.summaryPrefix")} <b>{selCount}</b>{" "}
                {t("repoManage.selection.summarySuffix")}{" "}
                <span className="dest">{p.baseFolderLabel}</span>
                {t("repoManage.selection.destinationHint")}
              </span>
              <span className="ident">
                <span className="gh">
                  <svg viewBox="0 0 16 16">
                    <path d="M8 0a8 8 0 00-2.5 15.6V14c-2 .4-2.5-.8-2.7-1.2-.1-.2-.5-.9-.8-1.1-.3-.1-.7-.5 0-.5.6 0 1 .6 1.2.8.7 1.2 1.9.9 2.3.7.1-.5.3-.9.5-1.1-1.8-.2-3.6-.9-3.6-4 0-.9.3-1.6.8-2.1-.1-.2-.4-1 .1-2.1 0 0 .7-.2 2.2.8a7.6 7.6 0 014 0c1.5-1 2.2-.8 2.2-.8.4 1.1.2 1.9.1 2.1.5.5.8 1.2.8 2.1 0 3.1-1.9 3.8-3.7 4 .3.2.5.7.5 1.5v2.2A8 8 0 008 0z" />
                  </svg>
                </span>
                {t("repoManage.selection.identity", {
                  login: p.selectedLogin,
                })}
              </span>
            </div>
            <button className="ob-btn primary" onClick={p.onClone}>
              <svg
                viewBox="0 0 24 24"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M12 3v12M7 10l5 5 5-5M5 21h14" />
              </svg>
              {t("repoManage.clone")}
            </button>
          </div>
        ) : (
          <>
            <button
              type="button"
              className="rm-foot-add"
              onClick={() => setConnectHintOpen((v) => !v)}
            >
              <svg viewBox="0 0 24 24" strokeWidth="2" strokeLinecap="round">
                <path d="M12 5v14M5 12h14" />
              </svg>
              {t("repoManage.connectAction")}
            </button>
            {connectHintOpen && (
              <div className="ob-gate-card">
                <div className="sub">
                  {t("repoManage.connect.instructions.prefix")}{" "}
                  <code>gh auth login</code>
                  {t("repoManage.connect.instructions.suffix")}
                </div>
                <button
                  className="ob-btn"
                  onClick={() => {
                    p.onRefreshAccounts();
                    setConnectHintOpen(false);
                  }}
                >
                  {t("repoManage.refresh")}
                </button>
              </div>
            )}
          </>
        )}
        <button
          type="button"
          className="rm-foot-add secondary"
          onClick={p.onConnectLocal}
        >
          <svg viewBox="0 0 24 24" strokeWidth="2" strokeLinecap="round">
            <path d="M12 5v14M5 12h14" />
          </svg>
          {t("repoManage.connectLocal")}
        </button>
        {p.connectError && (
          <div className="ob-gate-err" role="alert">
            {connectErrorText(p.connectError, t)}
          </div>
        )}
      </div>
    </>
  );
}
