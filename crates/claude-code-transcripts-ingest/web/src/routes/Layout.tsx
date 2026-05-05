import { ReactNode, useState } from "react";
import { NavLink, Outlet, useOutletContext } from "react-router-dom";

type LayoutContext = {
  setNavExtras: (node: ReactNode) => void;
};

export function Layout() {
  const [navExtras, setNavExtras] = useState<ReactNode>(null);

  return (
    <>
      <div className="header">
        <div className="tab-nav">
          <NavLink
            to="/dashboard"
            className={({ isActive }) => `tab-btn ${isActive ? "active" : ""}`}
          >
            Dashboard
          </NavLink>
          <NavLink
            to="/cost"
            className={({ isActive }) => `tab-btn ${isActive ? "active" : ""}`}
          >
            Cost
          </NavLink>
          <NavLink
            to="/sessions"
            className={({ isActive }) => `tab-btn ${isActive ? "active" : ""}`}
          >
            Sessions
          </NavLink>
        </div>
        {navExtras && <div className="nav-extras">{navExtras}</div>}
      </div>
      <Outlet context={{ setNavExtras } satisfies LayoutContext} />
    </>
  );
}

export function useLayoutContext(): LayoutContext {
  return useOutletContext<LayoutContext>();
}
