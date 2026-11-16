// IoT Edge's own interface at /edge/ (Ferrum docs/plan/edge.md): gateways and what they reach, devices,
// alarms, robots, definitions and flows, with the rest in the records frame's lists and forms.

import { useEffect } from "react";
import { Link, Outlet, useRouteError } from "react-router";
import { bootstrap, shell, t } from "@ferrum/client";
import { EmptyState } from "@ferrum/kit";
import { FailedState } from "./states";
import { mountApp, RequireAuth, setFrameMode } from "@ferrum/records/frame";
import { useFreshReadings } from "./live";
import "./styles.css";

const page = (name: "FormView" | "ListView" | "ReportView") => () => import("./records").then((m) => ({ Component: m[name] }));

function RouteError() {
  return <FailedState error={useRouteError()} onRetry={() => window.location.reload()} />;
}

function NotFound() {
  return (
    <EmptyState title={t("Page not found")}>
      <Link to="/">{t("Go to IoT Edge")}</Link>
    </EmptyState>
  );
}

function Started() {
  useEffect(() => shell().ready(), []);
  return null;
}

function Fresh() {
  useFreshReadings();
  return null;
}

/** The portal's page, when the platform serves this release there. */
const PORTAL = "portal/devices";
const onPortal = bootstrap()?.app?.entry === PORTAL;

if (!onPortal) setFrameMode("app", ["edge"]);

const portalRoutes = [
  {
    path: "/",
    element: (
      <main className="edge-page edge-portal">
        <Started />
        <Outlet />
      </main>
    ),
    errorElement: <RouteError />,
    children: [
      { index: true, lazy: () => import("./Portal").then((m) => ({ Component: m.PortalDevices })) },
      { path: "d/:id", lazy: () => import("./Portal").then((m) => ({ Component: m.PortalDevice })) },
      { path: "b/:id", lazy: () => import("./Portal").then((m) => ({ Component: m.PortalBoard })) },
      { path: "*", element: <NotFound /> },
    ],
  },
];

mountApp({
  route: onPortal ? PORTAL : "edge",
  routes: onPortal ? portalRoutes : [
    {
      path: "/",
      element: (
        <>
          <Started />
          <Fresh />
          <RequireAuth />
        </>
      ),
      errorElement: <RouteError />,
      children: [
        { index: true, lazy: () => import("./Home").then((m) => ({ Component: m.Home })) },
        { path: "devices", lazy: () => import("./Devices").then((m) => ({ Component: m.Devices })) },
        { path: "d/:id", lazy: () => import("./Device").then((m) => ({ Component: m.DevicePage })) },
        { path: "alarms", lazy: () => import("./Alarms").then((m) => ({ Component: m.Alarms })) },
        { path: "alarms/:id", lazy: () => import("./AlarmPage").then((m) => ({ Component: m.AlarmPage })) },
        { path: "robots", lazy: () => import("./Robots").then((m) => ({ Component: m.Robots })) },
        { path: "definitions", lazy: () => import("./Definitions").then((m) => ({ Component: m.Definitions })) },
        { path: "definitions/:id", lazy: () => import("./Definitions").then((m) => ({ Component: m.DefinitionPage })) },
        { path: "dashboards", lazy: () => import("./Boards").then((m) => ({ Component: m.Boards })) },
        { path: "dashboards/:id", lazy: () => import("./Boards").then((m) => ({ Component: m.BoardPage })) },
        { path: "solutions", lazy: () => import("./Solutions").then((m) => ({ Component: m.Solutions })) },
        { path: "updates", lazy: () => import("./Updates").then((m) => ({ Component: m.Updates })) },
        { path: "flows", lazy: () => import("./Flows").then((m) => ({ Component: m.Flows })) },
        { path: "flows/:id", lazy: () => import("./FlowEditor").then((m) => ({ Component: m.FlowEditor })) },
        { path: "e/:entity", lazy: page("ListView") },
        { path: "e/:entity/:id", lazy: page("FormView") },
        { path: "r/:name", lazy: page("ReportView") },
        { path: "*", element: <NotFound /> },
      ],
    },
  ],
});
