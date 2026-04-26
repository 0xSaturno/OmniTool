import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./ModelConverter.module.css";

import { FaArrowRight } from "react-icons/fa";
import { FaArrowLeft } from "react-icons/fa";

const MODEL_FILTER = [{ name: "Insomniac Model", extensions: ["model"] }];
const ASCII_FILTER = [{ name: "ASCII Model", extensions: ["ascii"] }];

type Tab = "to-ascii" | "to-model";

export default function ModelConverter() {
  const location = useLocation();
  const [tab, setTab] = useState<Tab>("to-ascii");

  const [modelPath, setModelPath] = useState("");
  const [asciiOutPath, setAsciiOutPath] = useState("");
  const [look, setLook] = useState(0);

  const [asciiPath, setAsciiPath] = useState("");
  const [srcModelPath, setSrcModelPath] = useState("");
  const [modelOutPath, setModelOutPath] = useState("");

  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    const fp = (location.state as { filePath?: string } | null)?.filePath;
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
  }, [location.state]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function runModelToAscii() {
    if (!modelPath) { pushLog("error", "Select a .model file first."); return; }
    setRunning(true);
    setLog([]);
    try {
      pushLog("info", `Converting ${modelPath} …`);
      const result: string = await invoke("model_to_ascii", {
        modelPath,
        asciiPath: asciiOutPath || null,
        look,
      });
      pushLog("success", `Done → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function runAsciiToModel() {
    if (!asciiPath)    { pushLog("error", "Select a .ascii file first."); return; }
    if (!srcModelPath) { pushLog("error", "Select a source .model file first."); return; }
    setRunning(true);
    setLog([]);
    try {
      pushLog("info", `Injecting ${asciiPath} → ${srcModelPath} …`);
      const result: string = await invoke("ascii_to_model", {
        asciiPath,
        srcModelPath,
        outPath: modelOutPath || null,
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
          ASCII <FaArrowLeft /> Model
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
            />
            <span className={styles.lookHint}>0 = primary appearance</span>
          </div>
          <button className={styles.runBtn} onClick={runModelToAscii} disabled={running}>
            {running ? "Converting…" : "Export to ASCII"}
          </button>
        </div>
      )}

      {tab === "to-model" && (
        <div className={styles.panel}>
          <FilePickerInput label="Source .ascii" value={asciiPath} onChange={setAsciiPath} mode="open" filters={ASCII_FILTER} />
          <FilePickerInput label="Base .model (original file to inject into)" value={srcModelPath} onChange={setSrcModelPath} mode="open" filters={MODEL_FILTER} />
          <FilePickerInput label="Output .model (optional — defaults to source with _modified suffix)" value={modelOutPath} onChange={setModelOutPath} mode="save" filters={MODEL_FILTER} placeholder="Leave blank for auto" />
          <button className={styles.runBtn} onClick={runAsciiToModel} disabled={running}>
            {running ? "Injecting…" : "Inject into Model"}
          </button>
        </div>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
