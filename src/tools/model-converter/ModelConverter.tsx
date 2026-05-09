import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./ModelConverter.module.css";

import { FaArrowRight } from "react-icons/fa";

const MODEL_FILTER = [{ name: "Insomniac Model", extensions: ["model"] }];
const ASCII_FILTER = [{ name: "ASCII Model", extensions: ["ascii"] }];

type Tab = "to-ascii" | "to-model";

interface LookGroupInfo {
  index: number;
  name: string;
}

export default function ModelConverter() {
  const location = useLocation();
  const [tab, setTab] = useState<Tab>("to-ascii");

  const [modelPath, setModelPath] = useState("");
  const [asciiOutPath, setAsciiOutPath] = useState("");
  const [look, setLook] = useState(0);

  const [lookGroups, setLookGroups] = useState<LookGroupInfo[] | null>(null);
  const [selectedLooks, setSelectedLooks] = useState<Set<number>>(new Set());
  const [loadingLookGroups, setLoadingLookGroups] = useState(false);

  const [asciiPath, setAsciiPath] = useState("");
  const [srcModelPath, setSrcModelPath] = useState("");
  const [modelOutPath, setModelOutPath] = useState("");
  const [overwriteSourceModel, setOverwriteSourceModel] = useState(false);

  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    const params = new URLSearchParams(location.search);
    const fp = (location.state as { filePath?: string } | null)?.filePath ?? params.get("filePath");
    if (!fp) return;
    // .model → pre-fill the model-to-ascii source; .ascii → pre-fill ascii-to-model source
    if (fp.toLowerCase().endsWith(".model")) {
      setModelPath(fp);
      setSrcModelPath(fp);
      setTab("to-ascii");
    } else if (fp.toLowerCase().endsWith(".ascii")) {
      setAsciiPath(fp);
      setTab("to-model");
    }
  }, [location.state, location.search]);

  // Reset look-group cache whenever the source model changes.
  useEffect(() => {
    setLookGroups(null);
    setSelectedLooks(new Set());
  }, [modelPath]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadLookGroups() {
    if (!modelPath) { pushLog("error", "Select a .model file first."); return; }
    setLoadingLookGroups(true);
    try {
      const groups: LookGroupInfo[] = await invoke("list_model_lookgroups", { modelPath });
      setLookGroups(groups);
      if (groups.length === 0) {
        pushLog("warning", "No look groups found in this model.");
      } else {
        pushLog("info", `Found ${groups.length} look group(s).`);
      }
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setLoadingLookGroups(false);
    }
  }

  function toggleLook(index: number) {
    setSelectedLooks((prev) => {
      const next = new Set(prev);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });
  }

  function selectAllLooks() {
    if (!lookGroups) return;
    setSelectedLooks(new Set(lookGroups.map((g) => g.index)));
  }

  function clearLookSelection() {
    setSelectedLooks(new Set());
  }

  async function runModelToAscii() {
    if (!modelPath) { pushLog("error", "Select a .model file first."); return; }

    // Determine which look indices to extract.
    // If the user picked any in the multi-select, use those; otherwise fall back to the single look number.
    const looks = selectedLooks.size > 0
      ? Array.from(selectedLooks).sort((a, b) => a - b)
      : [look];

    setRunning(true);
    setLog([]);
    try {
      if (looks.length === 1) {
        const lookIdx = looks[0];
        const groupName = lookGroups?.find((g) => g.index === lookIdx)?.name;
        pushLog("info", `Converting ${modelPath} (look ${lookIdx}${groupName ? ` "${groupName}"` : ""}) …`);
        const result: string = await invoke("model_to_ascii", {
          modelPath,
          asciiPath: asciiOutPath || null,
          look: lookIdx,
        });
        pushLog("success", `Done → ${result}`);
      } else {
        pushLog("info", `Converting ${modelPath} for ${looks.length} look group(s) into a single ASCII …`);
        const result: string = await invoke("model_to_ascii", {
          modelPath,
          asciiPath: asciiOutPath || null,
          looks,
        });
        pushLog("success", `Done → ${result}`);
      }
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function runAsciiToModel() {
    if (!asciiPath)    { pushLog("error", "Select a .ascii file first."); return; }
    if (!srcModelPath) { pushLog("error", "Select a source .model file first."); return; }
    const outputPath = overwriteSourceModel ? srcModelPath : (modelOutPath || null);
    setRunning(true);
    setLog([]);
    try {
      pushLog("info", `Injecting ${asciiPath} → ${srcModelPath} …`);
      const result: string = await invoke("ascii_to_model", {
        asciiPath,
        srcModelPath,
        outPath: outputPath,
      });
      pushLog("success", `Done → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Model Converter</h2>
      <p className={styles.subtitle}>Export .model mesh data to .ascii for editing, then inject it back</p>

      <div className={styles.tabs}>
        <button className={`${styles.tab} ${tab === "to-ascii" ? styles.active : ""}`} onClick={() => setTab("to-ascii")}>
          Model <FaArrowRight /> ASCII
        </button>
        <button className={`${styles.tab} ${tab === "to-model" ? styles.active : ""}`} onClick={() => setTab("to-model")}>
          ASCII <FaArrowRight /> Model
        </button>
      </div>

      {tab === "to-ascii" && (
        <div className={styles.panel}>
          <FilePickerInput label="Source .model" value={modelPath} onChange={setModelPath} mode="open" filters={MODEL_FILTER} />
          <FilePickerInput label="Output .ascii (optional — defaults to same folder)" value={asciiOutPath} onChange={setAsciiOutPath} mode="save" filters={ASCII_FILTER} placeholder="Leave blank for auto" />
          <div className={styles.lookRow}>
            <label className={styles.lookLabel}>Look group</label>
            <input
              type="number"
              min={0}
              value={look}
              onChange={(e) => setLook(Math.max(0, parseInt(e.target.value) || 0))}
              className={styles.lookInput}
              disabled={selectedLooks.size > 0}
            />
            <span className={styles.lookHint}>
              {selectedLooks.size > 0
                ? `Using ${selectedLooks.size} selected look group(s) below`
                : "0 = primary appearance"}
            </span>
          </div>

          <div className={styles.lookGroupsSection}>
            <div className={styles.lookGroupsHeader}>
              <button
                type="button"
                className={styles.listLookGroupsBtn}
                onClick={loadLookGroups}
                disabled={loadingLookGroups || !modelPath}
                title="Read all look groups from the selected .model"
              >
                {loadingLookGroups ? "Loading…" : lookGroups ? "Refresh Look Groups" : "List Look Groups"}
              </button>
              {lookGroups && lookGroups.length > 0 && (
                <div className={styles.lookGroupsActions}>
                  <button type="button" className={styles.linkBtn} onClick={selectAllLooks}>Select all</button>
                  <button type="button" className={styles.linkBtn} onClick={clearLookSelection}>Clear</button>
                </div>
              )}
            </div>

            {lookGroups && lookGroups.length > 0 && (
              <div className={styles.lookGroupsList}>
                {lookGroups.map((g) => (
                  <label key={g.index} className={styles.lookGroupItem}>
                    <input
                      type="checkbox"
                      checked={selectedLooks.has(g.index)}
                      onChange={() => toggleLook(g.index)}
                    />
                    <span className={styles.lookGroupIndex}>#{g.index}</span>
                    <span className={styles.lookGroupName}>{g.name || "(unnamed)"}</span>
                  </label>
                ))}
              </div>
            )}
          </div>

          <button className={styles.runBtn} onClick={runModelToAscii} disabled={running}>
            {running
              ? "Converting…"
              : selectedLooks.size > 1
                ? `Export ${selectedLooks.size} Look Groups to ASCII`
                : "Export to ASCII"}
          </button>
        </div>
      )}

      {tab === "to-model" && (
        <div className={styles.panel}>
          <FilePickerInput label="Source .ascii" value={asciiPath} onChange={setAsciiPath} mode="open" filters={ASCII_FILTER} />
          <FilePickerInput label="Base .model (original file to inject into)" value={srcModelPath} onChange={setSrcModelPath} mode="open" filters={MODEL_FILTER} />
          <FilePickerInput
            label="Output .model (optional — defaults to source with _modified suffix)"
            value={modelOutPath}
            onChange={setModelOutPath}
            mode="save"
            filters={MODEL_FILTER}
            placeholder="Leave blank for auto"
          />
          <div className={styles.injectActionsRow}>
            <button className={styles.runBtn} onClick={runAsciiToModel} disabled={running}>
              {running ? "Injecting…" : "Inject into Model"}
            </button>
            <label className={styles.overwriteToggle}>
              <input
                type="checkbox"
                checked={overwriteSourceModel}
                onChange={(e) => setOverwriteSourceModel(e.target.checked)}
              />
              <span>Overwrite source model</span>
            </label>
          </div>
        </div>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
