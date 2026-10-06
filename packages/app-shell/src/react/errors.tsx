import { Component, type ErrorInfo, type ReactNode } from "react";

export interface ErrorBoundaryProps {
  children: ReactNode;
  /** Custom fallback; receives the error and a reset function. */
  fallback?: (error: Error, reset: () => void) => ReactNode;
  onError?: (error: Error, info: ErrorInfo) => void;
  label?: string;
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
