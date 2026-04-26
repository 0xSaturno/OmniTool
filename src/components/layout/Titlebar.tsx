import { getCurrentWindow } from "@tauri-apps/api/window";
import { VscChromeMinimize, VscChromeMaximize, VscChromeRestore, VscChromeClose } from "react-icons/vsc";
import { useState, useEffect } from "react";
import { useLocation } from "react-router-dom";
import { TOOLS } from "../../tools/registry";
import styles from "./Titlebar.module.css";

export default function Titlebar() {
  const appWindow = getCurrentWindow();
  const location = useLocation();
  const [isMaximized, setIsMaximized] = useState(false);

  const currentTool = TOOLS.find((t) => t.path === location.pathname);

  useEffect(() => {
    // Check initial state
    appWindow.isMaximized().then(setIsMaximized);

    // Listen for resize events to update the maximize/restore icon
    const unlisten = appWindow.onResized(async () => {
      const maximized = await appWindow.isMaximized();
      setIsMaximized(maximized);
    });

    return () => {
      unlisten.then((f) => f());
    };
  }, [appWindow]);

  return (
    <div className={styles.titlebar}>
      <div className={styles.dragRegion} data-tauri-drag-region />
      <div className={styles.titleContainer}>
        {currentTool && (
          <>
            <span className={styles.titleIcon}>{currentTool.icon}</span>
            <span className={styles.titleText}>{currentTool.label}</span>
            <span className={styles.titleSeparator}>—</span>
          </>
        )}
        <span className={styles.titleText}>OmniTool</span>
      </div>
      <div className={styles.windowControls}>
        <button
          className={styles.controlBtn}
          onClick={() => appWindow.minimize()}
          title="Minimize"
        >
          <VscChromeMinimize />
        </button>
        <button
          className={styles.controlBtn}
          onClick={() => appWindow.toggleMaximize()}
          title={isMaximized ? "Restore" : "Maximize"}
        >
          {isMaximized ? <VscChromeRestore /> : <VscChromeMaximize />}
        </button>
        <button
          className={`${styles.controlBtn} ${styles.closeBtn}`}
          onClick={() => appWindow.close()}
          title="Close"
        >
          <VscChromeClose />
        </button>
      </div>
    </div>
  );
}
