import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { ErrorBoundary } from "./ErrorBoundary";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("index.html is missing the #root element the UI mounts into.");
createRoot(root).render(
  <StrictMode>
    <ErrorBoundary message="Drawer could not start">
      <App />
    </ErrorBoundary>
  </StrictMode>,
);
