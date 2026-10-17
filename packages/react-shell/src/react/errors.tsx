import { Component, type ErrorInfo, type ReactNode } from "react";

export interface ErrorBoundaryProps {
  children: ReactNode;
  /** Custom fallback; receives the error and a reset function. */
  fallback?: (error: Error, reset: () => void) => ReactNode;
  onError?: (error: Error, info: ErrorInfo) => void;
  label?: string;
  /**
   * When this value changes (compared with `Object.is`) while the boundary shows its fallback, the error clears and the
   * children render again. Pass the route, document or selected item id so navigating away recovers without a remount.
   */
  resetKey?: unknown;
}

/** Keeps one broken view from blanking the whole frame; the chrome stays usable. */
export class ErrorBoundary extends Component<ErrorBoundaryProps, { error: Error | null }> {
  state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  componentDidCatch(error: Error, info: ErrorInfo) {
    this.props.onError?.(error, info);
  }
  componentDidUpdate(prevProps: Readonly<ErrorBoundaryProps>) {
    if (this.state.error && !Object.is(prevProps.resetKey, this.props.resetKey)) this.setState({ error: null });
  }
  reset = () => this.setState({ error: null });
  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    if (this.props.fallback) return this.props.fallback(error, this.reset);
    return (
      <div className="rk-empty" role="alert">
        <strong>{this.props.label ?? "Something went wrong"}</strong>
        <p className="selectable">{error.message}</p>
        <button type="button" className="rk-btn rk-btn--secondary" onClick={this.reset}>Try again</button>
      </div>
    );
  }
}
