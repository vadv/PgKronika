import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./components/ActivityWorkspace.css";
import "./components/StatementsTimeMatrix.css";
import "./design/tokens.css";
import { applyTheme, resolveTheme } from "./design/theme";
import "./i18n";

// Boot applies the resolved theme without persisting it: only an explicit
// user choice may freeze the theme in localStorage, otherwise the UI keeps
// following prefers-color-scheme.
applyTheme(resolveTheme(), false);

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("missing #root element");
}

createRoot(rootElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
