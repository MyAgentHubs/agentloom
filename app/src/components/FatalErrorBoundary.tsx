import {
  Component,
  useEffect,
  useRef,
  type ErrorInfo,
  type ReactNode,
} from "react";
import { buildFatalErrorElement } from "../lib/fatalErrorPage";
import { reportFatalError } from "../lib/fatalErrorReport";

function FatalErrorFallback({ error }: { error: Error }) {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const el = buildFatalErrorElement({
      message: error.message,
      stack: error.stack,
    });
    host.appendChild(el);
    return () => {
      host.removeChild(el);
    };
  }, [error]);

  return <div ref={hostRef} data-testid="fatal-error-boundary-host" />;
}

interface FatalErrorBoundaryState {
  error: Error | null;
}

interface FatalErrorBoundaryProps {
  children: ReactNode;
}

/**
 * 三层兜底里的第二层：React 渲染树内的未捕获异常在这里被拦下，
 * 渲染同款报错页（复用 buildFatalErrorElement），并经 boot_trace 通道上报。
 */
export class FatalErrorBoundary extends Component<
  FatalErrorBoundaryProps,
  FatalErrorBoundaryState
> {
  state: FatalErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: unknown): FatalErrorBoundaryState {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    const err = error instanceof Error ? error : new Error(String(error));
    reportFatalError("ErrorBoundary", err.message);
    if (import.meta.env.DEV) {
      // eslint-disable-next-line no-console
      console.error("[FatalErrorBoundary]", err, info.componentStack);
    }
  }

  render(): ReactNode {
    if (this.state.error) {
      return <FatalErrorFallback error={this.state.error} />;
    }
    return this.props.children;
  }
}
