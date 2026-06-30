import { TOOLS } from "../tools/registry";
import { openToolWindow } from "../utils/openToolWindow";
import { useSettings } from "../contexts/SettingsContext";
import styles from "./Home.module.css";
import OmniToolIcon from "../components/icons/OmniToolIcon";

export default function Home() {
  const { settings } = useSettings();

  return (
    <div className={styles.home}>
      <h1 className={styles.title}><OmniToolIcon style={{ width: "1em", height: "1em", verticalAlign: "middle", marginTop: "-0.2em" }} /> OmniTool</h1>
      <p className={styles.subtitle}>
        Ratchet &amp; Clank: Rift Apart modding suite
      </p>

      <div className={styles.grid}>
        {TOOLS.map((tool) => {
          const isWIP = ["atmosphere-editor", "zonelightbin-module", "wwise-patcher", "bnk-explorer"].includes(tool.id);
          const isDisabled = tool.id === "zonelightbin-module";
          return (
            <button
              key={tool.id}
              className={`${styles.card} ${isWIP ? styles.cardWIP : ""} ${isDisabled ? styles.cardDisabled : ""}`}
              onClick={() => openToolWindow(tool.path, undefined, settings.launchToolsInNewWindows)}
              disabled={isDisabled}
            >
              <span className={styles.cardIcon}>{tool.icon}</span>
              <span className={styles.cardLabel}>{tool.label}</span>
              <span className={styles.cardDesc}>{tool.description}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
