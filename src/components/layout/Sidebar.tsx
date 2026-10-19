import { useState } from "react";
import { NavLink } from "react-router-dom";
import { open } from "@tauri-apps/plugin-shell";
import { message } from "@tauri-apps/plugin-dialog";
import { TOOLS, type ToolDefinition } from "../../tools/registry";
import { useSettings } from "../../contexts/SettingsContext";
import { openToolWindow } from "../../utils/openToolWindow";
import { useRivetHookInstalled } from "../../utils/useRivetHook";
import styles from "./Sidebar.module.css";
import OmniToolIcon from "../icons/OmniToolIcon";
import RivetHookIcon from "../icons/RivetHookIcon";
import { LuHouse, LuChevronsLeft, LuChevronsRight, LuSettings, LuGamepad2 } from "react-icons/lu";

const STEAM_LAUNCH_URL = "steam://rungameid/1895880";
const RIVET_MANAGER_PATH = "/tools/rivet-manager";

function launchGame() {
  open(STEAM_LAUNCH_URL).catch((e) =>
    message(`Could not launch the game through Steam: ${e}`, { title: "Launch Game", kind: "error" }),
  );
}

const CATEGORIES: { id: ToolDefinition["category"]; label: string }[] = [
  { id: "asset", label: "Asset" },
  { id: "model", label: "Model" },
  { id: "texture", label: "Texture" },
  { id: "audio", label: "Audio" },
  { id: "config", label: "Config" },
  { id: "misc", label: "Misc" },
];

export default function Sidebar() {
  const { settings, setSettingsOpen } = useSettings();
  const rivetMissing = useRivetHookInstalled(settings.archivesDir) === false;
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
      <div className={styles.brand}>
        <span className={styles.brandIcon}><OmniToolIcon style={{ width: "1.2rem", height: "1.2rem" }} /></span>
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
          {TOOLS.filter((t) => t.category === cat.id).map((tool) => {
            const isWIP = ["wwise-patcher", "bnk-explorer", "arena-editor"].includes(tool.id);
            const isDisabled = tool.disabled === true || (tool.devOnly === true && !import.meta.env.DEV);
            return (
              <NavLink
                key={tool.id}
                to={tool.path}
                onClick={(e) => {
                  e.preventDefault();
                  if (!isDisabled) {
                    openToolWindow(tool.path, undefined, settings.launchToolsInNewWindows);
                  }
                }}
                className={({ isActive }) => `${styles.navLink} ${isActive ? styles.active : ""} ${isWIP ? styles.navLinkWIP : ""} ${isDisabled ? styles.navLinkDisabled : ""}`}
                title={collapsed ? tool.label : undefined}
                style={isDisabled ? { pointerEvents: "none" } : undefined}
              >
                <span className={styles.navIcon}>{tool.icon}</span>
                <span className={styles.navLabel}>{tool.label}</span>
              </NavLink>
            );
          })}
        </div>
      ))}

      <div className={styles.spacer} />

      <NavLink
        to={RIVET_MANAGER_PATH}
        onClick={(e) => {
          e.preventDefault();
          if (!rivetMissing) {
            openToolWindow(RIVET_MANAGER_PATH, undefined, settings.launchToolsInNewWindows);
          }
        }}
        className={({ isActive }) =>
          `${styles.navLink} ${styles.settingsBtn} ${isActive ? styles.active : ""} ${rivetMissing ? styles.navLinkUnavailable : ""}`
        }
        aria-disabled={rivetMissing}
        title={
          rivetMissing
            ? settings.archivesDir
              ? "Rivet Hook not detected: rivet_hook.dll is not in the game folder"
              : "Set the game folder in Settings to use Rivet Hook"
            : collapsed
              ? "RivetHook Manager"
              : undefined
        }
      >
        <span className={styles.navIcon}><RivetHookIcon /></span>
        <span className={styles.navLabel}>RivetHook Manager</span>
      </NavLink>

      <button
        className={`${styles.navLink} ${styles.settingsBtn}`}
        onClick={launchGame}
        title={collapsed ? "Launch Game" : undefined}
      >
        <span className={styles.navIcon}><LuGamepad2 /></span>
        <span className={styles.navLabel}>Launch Game</span>
      </button>

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
