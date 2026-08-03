import type { ReactNode } from "react";

/* eslint-disable jsx-a11y/no-noninteractive-tabindex -- The fixed status
 * context is an explicit keyboard stop in the forensic-shell contract. */

export interface ShellLayoutProps {
  mobile: boolean;
  globalContext: ReactNode;
  primaryNavigation: ReactNode;
  primaryNavigationLabel: string;
  skipToMainLabel: string;
  status: ReactNode;
  overlay?: ReactNode;
  children: ReactNode;
}

export function ShellLayout(props: ShellLayoutProps) {
  const desktop = !props.mobile;
  return (
    <div
      data-testid="app-shell"
      data-shell-layout={desktop ? "desktop" : "mobile"}
      style={{
        height: desktop ? "100dvh" : undefined,
        minHeight: desktop ? 0 : undefined,
        display: desktop ? "flex" : "block",
        flexDirection: desktop ? "column" : undefined,
        overflow: desktop ? "hidden" : undefined,
        background: "var(--bg)",
        color: "var(--fg)",
      }}
    >
      <a
        className="skip-link"
        href="#main-content"
        onClick={(event) => {
          // UI state is encoded in location.hash; native fragment navigation
          // would replace the forensic route. Move focus without mutating it.
          event.preventDefault();
          document.getElementById("main-content")?.focus();
        }}
      >
        {props.skipToMainLabel}
      </a>
      <header
        data-shell-region="global-context"
        style={{
          height: desktop ? "44px" : undefined,
          flex: desktop ? "0 0 44px" : undefined,
          minWidth: 0,
        }}
      >
        {props.globalContext}
      </header>
      {props.primaryNavigation !== null && (
        <nav
          data-shell-region="primary-navigation"
          aria-label={props.primaryNavigationLabel}
          style={{
            height: desktop ? "32px" : undefined,
            flex: desktop ? "0 0 32px" : undefined,
            minWidth: 0,
            borderBottom: "1px solid var(--border)",
          }}
        >
          {props.primaryNavigation}
        </nav>
      )}
      <main
        id="main-content"
        tabIndex={-1}
        data-shell-region="main"
        style={{
          minHeight: desktop ? 0 : undefined,
          flex: desktop ? "1 1 auto" : undefined,
          display: desktop ? "flex" : undefined,
          flexDirection: desktop ? "column" : undefined,
          overflow: desktop ? "hidden" : "visible",
        }}
      >
        {props.children}
      </main>
      <footer
        data-shell-region="status"
        tabIndex={desktop ? 0 : undefined}
        style={{
          height: desktop ? "24px" : undefined,
          flex: desktop ? "0 0 24px" : undefined,
          minWidth: 0,
        }}
      >
        {props.status}
      </footer>
      {props.overlay}
    </div>
  );
}
