// Catches a viewer or the whole app throwing: a broken parser must not take the page with it.
import { Component, ErrorInfo, ReactNode } from "react";
import { Icon } from "./icons";

type Props = {
  children: ReactNode;
  /** Shown instead of the default heading, e.g. "This folder can't be listed". */
  message?: string;
  /** Renders a Download button when the file can still be fetched. */
  onDownload?: () => void;
  /** Label for the download button. */
  downloadLabel?: string;
};

type State = { error: Error | null };

export class ErrorBoundary extends Component<Props, State> {
  override state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  override componentDidCatch(error: Error, info: ErrorInfo) {
    // The message is what people need; the component stack names the viewer that broke.
    console.error("Drawer UI failed to render", error, info.componentStack);
  }

  override render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="notice">
        <Icon name="file" size={40} />
        <h3>{this.props.message ?? "This file can't be previewed"}</h3>
        <p className="muted">{error.message}</p>
        {this.props.onDownload && (
          <button className="btn primary" onClick={this.props.onDownload}>
            <Icon name="download" size={18} /> {this.props.downloadLabel ?? "Download"}
          </button>
        )}
      </div>
    );
  }
}
