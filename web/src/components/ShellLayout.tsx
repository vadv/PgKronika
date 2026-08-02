import type { ReactNode } from "react";

export interface ShellLayoutProps {
  mobile: boolean;
  globalContext: ReactNode;
  primaryNavigation: ReactNode;
  primaryNavigationLabel: string;
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
        minHeight: desktop ? "100dvh" : undefined,
        display: desktop ? "flex" : "block",
        flexDirection: desktop ? "column" : undefined,
        background: "var(--bg)",
        color: "var(--fg)",
      }}
    >
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
        data-shell-region="main"
        style={{
          minHeight: desktop ? 0 : undefined,
          flex: desktop ? "1 1 auto" : undefined,
          overflow: "visible",
        }}
      >
        {props.children}
      </main>
      <footer
        data-shell-region="status"
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
