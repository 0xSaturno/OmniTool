import { useState } from "react";
import { NavLink } from "react-router-dom";
import { TOOLS, type ToolDefinition } from "../../tools/registry";
import { useSettings } from "../../contexts/SettingsContext";
import styles from "./Sidebar.module.css";
import { BsWrenchAdjustableCircle } from "react-icons/bs";
import { LuHouse, LuChevronsLeft, LuChevronsRight, LuSettings } from "react-icons/lu";

const CATEGORIES: { id: ToolDefinition["category"]; label: string }[] = [
  { id: "model", label: "Model" },
  { id: "animation", label: "Animation" },
  { id: "archive", label: "Archive" },
  { id: "config", label: "Config" },
  { id: "misc", label: "Misc" },
];

export default function Sidebar() {
  const { setSettingsOpen } = useSettings();
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem("rcra_sidebar_collapsed") === "true"
  );

  const toggle = () =>
    setCollapsed((c) => {
      localStorage.setItem("rcra_sidebar_collapsed", String(!c));
      return !c;
    });

  return (
    <nav className={`${styles.sidebar} ${collapsed ? styles.collapsed : ""}`}>
      {/* Brand: icon + name fade, toggle always right-aligned */}
      <div className={styles.brand}>
        <span className={styles.brandIcon}><BsWrenchAdjustableCircle /></span>
        <span className={styles.brandName}>OmniTool</span>
        <button
          className={styles.collapseBtn}
          onClick={toggle}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        >
          {collapsed ? <LuChevronsRight /> : <LuChevronsLeft />}
        </button>
      </div>

      <NavLink
        to="/"
        className={({ isActive }) => `${styles.navLink} ${isActive ? styles.active : ""}`}
        end
        title={collapsed ? "Home" : undefined}
      >
        <span className={styles.navIcon}><LuHouse /></span>
        <span className={styles.navLabel}>Home</span>
      </NavLink>

      {CATEGORIES.filter((cat) => TOOLS.some((t) => t.category === cat.id)).map((cat) => (
        <div key={cat.id} className={styles.group}>
          <span className={styles.groupLabel}>{cat.label}</span>
          {TOOLS.filter((t) => t.category === cat.id).map((tool) => (
            <NavLink
              key={tool.id}
              to={tool.path}
              className={({ isActive }) => `${styles.navLink} ${isActive ? styles.active : ""}`}
              title={collapsed ? tool.label : undefined}
            >
              <span className={styles.navIcon}>{tool.icon}</span>
              <span className={styles.navLabel}>{tool.label}</span>
            </NavLink>
          ))}
        </div>
      ))}

      <div className={styles.spacer} />

      <button
        className={`${styles.navLink} ${styles.settingsBtn}`}
        onClick={() => setSettingsOpen(true)}
        title={collapsed ? "Settings" : undefined}
      >
        <span className={styles.navIcon}><LuSettings /></span>
        <span className={styles.navLabel}>Settings</span>
      </button>
    </nav>
  );
}
