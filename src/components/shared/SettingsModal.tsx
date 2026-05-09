import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useSettings } from "../../contexts/SettingsContext";
import styles from "./SettingsModal.module.css";

export default function SettingsModal() {
  const { settings, updateSettings, isSettingsOpen, setSettingsOpen } = useSettings();

  const [hashesPath, setHashesPath] = useState("");
  const [hashesExist, setHashesExist] = useState<boolean | null>(null);
  const [fetchState, setFetchState] = useState<"idle" | "fetching" | "done" | "error">("idle");
  const [fetchMsg, setFetchMsg] = useState("");

  useEffect(() => {
    if (!isSettingsOpen) return;
    invoke<string>("get_hashes_path").then((p) => {
      setHashesPath(p);
      invoke<boolean>("hashes_exist").then(setHashesExist).catch(() => setHashesExist(false));
    });
  }, [isSettingsOpen]);

  if (!isSettingsOpen) return null;

  async function pickArchivesDir() {
    const result = await open({ directory: true, title: "Select Game Archives Folder" });
    if (typeof result === "string") {
      updateSettings({ archivesDir: result });
    }
  }

  async function fetchHashes() {
    setFetchState("fetching");
    setFetchMsg("");
    try {
      const result: string = await invoke("download_hashes");
      setFetchState("done");
      setFetchMsg(result);
      setHashesExist(true);
    } catch (e) {
      setFetchState("error");
      setFetchMsg(String(e));
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
            <p className={styles.hint}>Used by Asset Browser to load game files</p>
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
            <label>Asset Hashes</label>
            <div className={styles.hashesRow}>
              <span className={styles.hashesPath}>{hashesPath || "…"}</span>
              <span className={`${styles.hashBadge} ${hashesExist ? styles.badgeOk : styles.badgeMissing}`}>
                {hashesExist === null ? "…" : hashesExist ? "Present" : "Missing"}
              </span>
            </div>
            <div className={styles.fetchRow}>
              <button
                className={styles.fetchBtn}
                onClick={fetchHashes}
                disabled={fetchState === "fetching"}
              >
                {fetchState === "fetching" ? "Downloading…" : "Fetch from GitHub"}
              </button>
              {fetchMsg && (
                <span className={fetchState === "error" ? styles.fetchError : styles.fetchOk}>
                  {fetchState === "done" ? `✓ ${fetchMsg}` : fetchMsg}
                </span>
              )}
            </div>
            <p className={styles.hint}>
              Hash map used to resolve asset IDs to readable paths in the Asset Browser.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
