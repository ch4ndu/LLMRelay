import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App, applyAppearance, storedAppearance } from "./App";
import { AppErrorBoundary } from "./components/AppErrorBoundary";
import "./styles.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("LLMRelay dashboard root is missing");
}

applyAppearance(storedAppearance());

createRoot(root).render(
  <StrictMode>
    <AppErrorBoundary>
      <App />
    </AppErrorBoundary>
  </StrictMode>,
);
