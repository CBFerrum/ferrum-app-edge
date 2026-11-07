// Edge's own screens, at the top of each.

import { NavLink } from "react-router";
import { t } from "@ferrum/client";

export function Tabs({ alarms }: { alarms?: number }) {
  return (
    <nav className="edge-tabs" aria-label={t("IoT Edge")}>
      <NavLink to="/" end>
        {t("Gateways")}
      </NavLink>
      <NavLink to="/dashboards">{t("Dashboards")}</NavLink>
      <NavLink to="/devices">{t("Devices")}</NavLink>
      <NavLink to="/alarms">{t("Alarms")}{alarms ? ` (${alarms})` : ""}</NavLink>
      <NavLink to="/robots">{t("Robots")}</NavLink>
      <NavLink to="/flows">{t("Flows")}</NavLink>
      <NavLink to="/definitions">{t("Definitions")}</NavLink>
      <NavLink to="/updates">{t("Updates")}</NavLink>
      <NavLink to="/solutions">{t("Solutions")}</NavLink>
    </nav>
  );
}
