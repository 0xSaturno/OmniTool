import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import { useSettings } from "../../contexts/SettingsContext";
import styles from "./WwisePatcher.module.css";

const BNK_FILTER = [{ name: "Wwise Soundbank Payload (*.bnk)", extensions: ["bnk"] }];
const XML_FILTER = [{ name: "Soundbank Info (*.xml)", extensions: ["xml"] }];
const SOUNDBANK_FILTER = [{ name: "Asset Soundbank (*.soundbank)", extensions: ["soundbank"] }];
const LOOKUP_FILTER = [{ name: "Wwise Lookup (*.wwiselookup)", extensions: ["wwiselookup"] }];

interface PatchEntry {
  bankPath: string;
  eventNames: string[];
}

export default function WwisePatcher() {
  const { settings } = useSettings();
  const [tab, setTab] = useState<"build" | "patch">("build");
  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  // --- Build Soundbank State ---
  const [bnkPath, setBnkPath] = useState("");
  const [xmlPath, setXmlPath] = useState("");
  const [assetPath, setAssetPath] = useState("sound/banks/custom_bank.soundbank");
  const [outputPath, setOutputPath] = useState("");

  // --- Patch Registry State ---
  const [vanillaLookup, setVanillaLookup] = useState("");
  const [destLookup, setDestLookup] = useState("");
  const [patchEntries, setPatchEntries] = useState<PatchEntry[]>([]);

  // Individual entry inputs
  const [entryBankPath, setEntryBankPath] = useState("");
  const [entryEventsText, setEntryEventsText] = useState("");

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  // Pre-fill outputPath based on bnkPath and assetPath
  useEffect(() => {
    if (bnkPath && !outputPath) {
      // Suggest .soundbank in the same directory as .bnk
      const idx = bnkPath.lastIndexOf(".");
      const base = idx !== -1 ? bnkPath.substring(0, idx) : bnkPath;
      setOutputPath(`${base}.soundbank`);
    }
  }, [bnkPath]);

  // Pre-fill lookup paths if settings has archivesDir
  useEffect(() => {
    if (settings.archivesDir) {
      // We can try to guess where vanilla events.wwiselookup could be extracted from,
      // or at least auto-populate a default custom lookup path in a mod project.
      if (!destLookup) {
        setDestLookup(`${settings.archivesDir}\\custom_events.wwiselookup`);
      }
    }
  }, [settings.archivesDir]);

  async function handleBuild() {
    if (!bnkPath || !xmlPath || !assetPath || !outputPath) {
      pushLog("error", "Please fill in all build fields.");
      return;
    }

    setRunning(true);
    pushLog("info", "Starting Soundbank build process...");

    try {
      await invoke("soundbank_build", {
        bnkPath,
        xmlPath,
        assetPath,
        outputPath,
      });
      pushLog("success", `Soundbank successfully built and saved to:\n${outputPath}`);
    } catch (e) {
      pushLog("error", `Failed to build soundbank: ${e}`);
    } finally {
      setRunning(false);
    }
  }

  function addPatchEntry() {
    if (!entryBankPath.trim()) {
      pushLog("error", "Soundbank Asset Path cannot be empty.");
      return;
    }

    const events = entryEventsText
      .split("\n")
      .map((e) => e.trim())
      .filter((e) => e.length > 0);

    if (events.length === 0) {
      pushLog("error", "Please specify at least one event name.");
      return;
    }

    // Check for duplicates
    if (patchEntries.some((e) => e.bankPath.toLowerCase() === entryBankPath.trim().toLowerCase())) {
      pushLog("error", `An entry for ${entryBankPath} already exists.`);
      return;
    }

    setPatchEntries((prev) => [
      ...prev,
      {
        bankPath: entryBankPath.trim(),
        eventNames: events,
      },
    ]);

    setEntryBankPath("");
    setEntryEventsText("");
    pushLog("success", `Added soundbank entry for "${entryBankPath.trim()}" with ${events.length} events.`);
  }

  function removePatchEntry(index: number) {
    const entry = patchEntries[index];
    setPatchEntries((prev) => prev.filter((_, i) => i !== index));
    pushLog("info", `Removed soundbank entry for "${entry.bankPath}".`);
  }

  async function importFromXml() {
    try {
      const selected = await open({
        filters: XML_FILTER,
        multiple: false,
        title: "Import Soundbanks from SoundBanksInfo.xml",
      });

      if (typeof selected !== "string") return;

      pushLog("info", `Reading soundbank metadata from ${selected}...`);
      const parsed: any[] = await invoke("soundbank_parse", { xmlPath: selected });

      let addedCount = 0;
      let skippedCount = 0;

      const newEntries = [...patchEntries];

      for (const bank of parsed) {
        const path = `sound/banks/${bank.bank_name.toLowerCase()}.soundbank`;
        const events = bank.events.map((e: any) => e.name);

        if (events.length === 0) continue;

        if (newEntries.some((e) => e.bankPath.toLowerCase() === path.toLowerCase())) {
          skippedCount++;
          continue;
        }

        newEntries.push({
          bankPath: path,
          eventNames: events,
        });
        addedCount++;
      }

      setPatchEntries(newEntries);
      pushLog(
        "success",
        `Successfully imported: ${addedCount} banks added, ${skippedCount} skipped (already in list).`
      );
    } catch (e) {
      pushLog("error", `Failed to parse XML: ${e}`);
    }
  }

  async function handlePatch() {
    if (!vanillaLookup || !destLookup) {
      pushLog("error", "Please specify base and destination lookup file paths.");
      return;
    }

    if (patchEntries.length === 0) {
      pushLog("error", "Please add at least one soundbank entry to patch.");
      return;
    }

    setRunning(true);
    pushLog("info", "Patching wwiselookup table...");

    // Format new assets parameter as Vec<(String, Vec<String>)>
    const newAssets = patchEntries.map((e) => [e.bankPath, e.eventNames]);

    try {
      await invoke("wwiselookup_patch", {
        vanillaPath: vanillaLookup,
        outputPath: destLookup,
        newAssets,
      });
      pushLog("success", `Registry wwiselookup table successfully patched to:\n${destLookup}`);
    } catch (e) {
      pushLog("error", `Failed to patch wwiselookup registry: ${e}`);
    } finally {
      setRunning(false);
    }
  }

  // Suggest default vanilla lookup path if we have archives folder
  async function tryAutoVanilla() {
    if (!settings.archivesDir) {
      pushLog("error", "Set the game archives directory in Settings first.");
      return;
    }

    // Usually, events.wwiselookup is extracted to temp/project. Let's see if we can check if it exists in the live archives path.
    // However, the cleanest way is for the user to extract it first or browse.
    // If they have the asset browser loaded, we can tell them where to find it.
    pushLog("info", "Tip: Extract 'sound/events.wwiselookup' via the Asset Browser, then select it as the Base Lookup File.");
  }

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Wwise Soundbank Patcher</h2>
      <p className={styles.subtitle}>Build custom soundbanks and patch the events.wwiselookup table</p>

      <div className={styles.tabs}>
        <button
          className={`${styles.tab} ${tab === "build" ? styles.active : ""}`}
          onClick={() => setTab("build")}
        >
          Build Soundbank (.soundbank)
        </button>
        <button
          className={`${styles.tab} ${tab === "patch" ? styles.active : ""}`}
          onClick={() => setTab("patch")}
        >
          Patch Registry (wwiselookup)
        </button>
      </div>

      <div className={styles.panel}>
        {tab === "build" ? (
          <div className={styles.formGrid}>
            <div className={styles.section}>
              <h3 className={styles.sectionTitle}>Input Files</h3>
              <FilePickerInput
                label="Wwise Payload (.bnk)"
                value={bnkPath}
                onChange={setBnkPath}
                mode="open"
                filters={BNK_FILTER}
              />
              <FilePickerInput
                label="SoundBanksInfo.xml"
                value={xmlPath}
                onChange={setXmlPath}
                mode="open"
                filters={XML_FILTER}
              />
            </div>

            <div className={styles.section}>
              <h3 className={styles.sectionTitle}>Build Settings</h3>
              <div className={styles.inputGroup}>
                <label>Soundbank Asset Path</label>
                <input
                  type="text"
                  className={styles.textInput}
                  value={assetPath}
                  onChange={(e) => setAssetPath(e.target.value)}
                  placeholder="sound/banks/my_soundbank.soundbank"
                />
              </div>
              <FilePickerInput
                label="Output Destination (.soundbank)"
                value={outputPath}
                onChange={setOutputPath}
                mode="save"
                filters={SOUNDBANK_FILTER}
              />

              <div style={{ marginTop: "1rem", fontSize: "0.75rem", color: "var(--text-muted)", background: "var(--surface-2)", padding: "0.5rem", borderRadius: "4px" }}>
                <strong>Note:</strong> Rift Apart requires hardcoded Project ID <code>0x187E</code> for custom soundbanks. This tool automatically patches the Project ID in the BKHD header during compilation.
              </div>

              <div className={styles.actions}>
                <button
                  className={styles.runBtn}
                  onClick={handleBuild}
                  disabled={running || !bnkPath || !xmlPath || !outputPath}
                >
                  {running ? "Building..." : "Build Soundbank"}
                </button>
              </div>
            </div>
          </div>
        ) : (
          <div className={styles.formGrid}>
            <div className={styles.section}>
              <div style={{ display: "flex", justifyContent: "space-between", borderBottom: "1px solid var(--border)", paddingBottom: "0.5rem", alignItems: "center", marginBottom: "0.5rem" }}>
                <h3 style={{ fontSize: "0.9rem", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.05em", color: "var(--text-secondary)", margin: 0 }}>Patch Targets</h3>
                <button
                  className={styles.secondaryBtn}
                  style={{ padding: "0.15rem 0.4rem", fontSize: "0.68rem" }}
                  onClick={tryAutoVanilla}
                >
                  How to get Base File
                </button>
              </div>
              <FilePickerInput
                label="Base Lookup File (events.wwiselookup)"
                value={vanillaLookup}
                onChange={setVanillaLookup}
                mode="open"
                filters={LOOKUP_FILTER}
              />
              <FilePickerInput
                label="Destination Lookup File (events.wwiselookup)"
                value={destLookup}
                onChange={setDestLookup}
                mode="save"
                filters={LOOKUP_FILTER}
              />

              <h3 className={styles.sectionTitle} style={{ marginTop: "0.5rem" }}>Add Custom Bank Registry</h3>
              <div className={styles.inputGroup}>
                <label>Soundbank Asset Path</label>
                <input
                  type="text"
                  className={styles.textInput}
                  value={entryBankPath}
                  onChange={(e) => setEntryBankPath(e.target.value)}
                  placeholder="sound/banks/my_soundbank.soundbank"
                />
              </div>
              <div className={styles.inputGroup}>
                <label>Event Names (One per line)</label>
                <textarea
                  className={styles.textArea}
                  value={entryEventsText}
                  onChange={(e) => setEntryEventsText(e.target.value)}
                  placeholder="play_my_custom_event&#10;stop_my_custom_event"
                />
              </div>
              <div className={styles.actions} style={{ marginTop: 0 }}>
                <button className={styles.secondaryBtn} onClick={addPatchEntry}>
                  Add to List
                </button>
                <button className={styles.secondaryBtn} onClick={importFromXml}>
                  Import from SoundBanksInfo.xml
                </button>
              </div>
            </div>

            <div className={styles.section}>
              <h3 className={styles.sectionTitle}>Registered Soundbanks ({patchEntries.length})</h3>
              <div className={styles.listContainer}>
                <div className={styles.entryList}>
                  {patchEntries.length === 0 ? (
                    <div className={styles.emptyList}>
                      No custom soundbanks added to registry yet. Add them manually or import from SoundBanksInfo.xml.
                    </div>
                  ) : (
                    patchEntries.map((entry, index) => (
                      <div key={index} className={styles.entryItem}>
                        <div className={styles.entryInfo}>
                          <span className={styles.entryPath}>{entry.bankPath}</span>
                          <span className={styles.entryEventsCount}>{entry.eventNames.length} events</span>
                        </div>
                        <button
                          className={styles.removeBtn}
                          onClick={() => removePatchEntry(index)}
                          title="Remove from list"
                        >
                          Remove
                        </button>
                      </div>
                    ))
                  )}
                </div>
              </div>

              <div className={styles.actions}>
                <button
                  className={styles.runBtn}
                  onClick={handlePatch}
                  disabled={running || patchEntries.length === 0 || !vanillaLookup || !destLookup}
                >
                  {running ? "Patching..." : "Patch Registry Table"}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>

      <div className={styles.logContainer}>
        <StatusLog entries={log} />
      </div>
    </div>
  );
}
