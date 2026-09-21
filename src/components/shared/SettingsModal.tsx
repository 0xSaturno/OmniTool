import { open } from "@tauri-apps/plugin-dialog";
import { useSettings } from "../../contexts/SettingsContext";
import styles from "./SettingsModal.module.css";

export default function SettingsModal() {
  const { settings, updateSettings, isSettingsOpen, setSettingsOpen } = useSettings();

  if (!isSettingsOpen) return null;

  async function pickArchivesDir() {
    const result = await open({ directory: true, title: "Select Game Archives Folder" });
    if (typeof result === "string") {
      updateSettings({ archivesDir: result });
    }
  }

  async function pickOverstrikeDir() {
    const result = await open({ directory: true, title: "Select Overstrike Folder" });
    if (typeof result === "string") {
      updateSettings({ overstrikeDir: result });
    }
  }

  return (
    <div className={styles.overlay} onClick={() => setSettingsOpen(false)}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        <header className={styles.header}>
          <h2>Settings</h2>
          <button className={styles.closeBtn} onClick={() => setSettingsOpen(false)}>✕</button>
        </header>

        <div className={styles.content}>
          <div className={styles.field}>
            <label>Game Archives Folder</label>
            <div className={styles.inputGroup}>
              <input
                type="text"
                value={settings.archivesDir}
                readOnly
                placeholder="Select folder containing toc file"
              />
              <button className={styles.browseBtn} onClick={pickArchivesDir}>Browse</button>
            </div>
            <p className={styles.hint}>
              Used to load game files. Asset names come from the game's <code>dag</code> file in
              this folder.
            </p>
          </div>

          <div className={styles.field}>
            <label>Overstrike Folder</label>
            <div className={styles.inputGroup}>
              <input
                type="text"
                value={settings.overstrikeDir}
                readOnly
                placeholder="Select Overstrike folder (or its Mods Library)"
              />
              <button className={styles.browseBtn} onClick={pickOverstrikeDir}>Browse</button>
            </div>
            <p className={styles.hint}>
              Optional. Reads asset names out of the <code>.stage</code> packages so mod-added
              assets show real paths instead of <code>[UNKNOWN]</code>.
            </p>
          </div>

          <div className={styles.field}>
            <label className={styles.toggleRow}>
              <input
                type="checkbox"
                checked={settings.launchToolsInNewWindows}
                onChange={(e) => updateSettings({ launchToolsInNewWindows: e.target.checked })}
              />
              <span>Launch tools in separate windows</span>
            </label>
            <p className={styles.hint}>
              When disabled, tools open in a single unified window instead.
            </p>
          </div>

          <div className={styles.field}>
            <label className={styles.toggleRow}>
              <input
                type="checkbox"
                checked={settings.experimentalTiffExport}
                onChange={(e) => updateSettings({ experimentalTiffExport: e.target.checked })}
              />
              <span>Experimental: TIFF export for HDR textures</span>
            </label>
            <p className={styles.hint}>
              When enabled, BC6H and other HDR formats export as 32-bit float TIFF.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
