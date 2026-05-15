import { useState, useCallback, useRef, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { useLocation } from "react-router-dom";
import CodeMirror, { EditorView } from "@uiw/react-codemirror";
import { json } from "@codemirror/lang-json";
import { vscodeDark } from "@uiw/codemirror-theme-vscode";
import FilePickerInput from "../../components/shared/FilePickerInput";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./ConfigEditor.module.css";

const CONFIG_FILTER = [{ name: "Insomniac Config", extensions: ["config", "actor", "conduit", "performanceset"] }];

interface ConfigData {
  config_type: string;
  content_json: string;
  can_save: boolean;
}

export default function ConfigEditor() {
  const location = useLocation();
  const [configPath, setConfigPath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [overwriteInput, setOverwriteInput] = useState(false);
  const [sendToStager, setSendToStager] = useState<string | null>(null);
  const [assetPath, setAssetPath] = useState<string | null>(null);

  const [configType, setConfigType] = useState("");
  const [jsonText, setJsonText] = useState("");
  const [jsonError, setJsonError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [canSave, setCanSave] = useState(true);

  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  // Measure the border div after the editor renders so CodeMirror gets a
  useEffect(() => {
    if (location.pathname !== "/tools/config-editor") return;
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string; assetPath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    const incomingAssetPath = s?.assetPath ?? params.get("assetPath") ?? undefined;
    if (filePath) setConfigPath(filePath);
    if (incomingAssetPath) setAssetPath(incomingAssetPath);
  }, [location.pathname, location.state, location.search]);

  // concrete pixel height — required for its internal scroller to activate.
  const borderRef = useRef<HTMLDivElement>(null);
  const [cmHeight, setCmHeight] = useState("400px");

  useEffect(() => {
    if (!loaded) return;
    const el = borderRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      setCmHeight(`${Math.floor(entry.contentRect.height)}px`);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [loaded]);

  // Ctrl+scroll to zoom, Ctrl+0 to reset
  const [fontSize, setFontSize] = useState(13);
  const fontTheme = useMemo(
    () => EditorView.theme({ "&": { fontSize: `${fontSize}px` } }),
    [fontSize],
  );

  useEffect(() => {
    if (!loaded) return;
    const el = borderRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      setFontSize((prev) => Math.max(8, Math.min(32, prev + (e.deltaY < 0 ? 1 : -1))));
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (!e.ctrlKey) return;
      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        setFontSize((prev) => Math.min(32, prev + 1));
      } else if (e.key === "-") {
        e.preventDefault();
        setFontSize((prev) => Math.max(8, prev - 1));
      } else if (e.key === "0") {
        e.preventDefault();
        setFontSize(13);
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    el.addEventListener("keydown", onKeyDown);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("keydown", onKeyDown);
    };
  }, [loaded]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadConfig() {
    if (!configPath) {
      pushLog("error", "Select a supported config-like file first (.config/.actor/.conduit/.performanceset).");
      return;
    }
    setRunning(true);
    setLog([]);
    setLoaded(false);
    setJsonError("");
    try {
      pushLog("info", `Reading ${configPath} …`);
      const result: ConfigData = await invoke("read_config", { configPath });
      setConfigType(result.config_type);
      setJsonText(result.content_json);
      setCanSave(result.can_save);
      setLoaded(true);
      pushLog("success", `Loaded config type: ${result.config_type}`);
      if (!result.can_save) {
        pushLog("warning", "This format is currently read-only in OmniTool.");
      }
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function exportJson() {
    if (!loaded) return;
    if (jsonError) {
      pushLog("error", "Fix the JSON errors before exporting.");
      return;
    }

    const baseName = configPath
      ? configPath.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") ?? "config"
      : "config";

    const exportPath = await save({
      title: "Export Config as JSON",
      defaultPath: `${baseName}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!exportPath) return;

    setRunning(true);
    try {
      pushLog("info", "Exporting JSON …");
      const result: string = await invoke("export_config_json", {
        contentJson: jsonText,
        outPath: exportPath,
      });
      pushLog("success", `Exported JSON → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function onJsonChange(text: string) {
    setJsonText(text);
    try {
      JSON.parse(text);
      setJsonError("");
    } catch (e) {
      setJsonError(String(e));
    }
  }

  async function saveConfig() {
    if (!configPath || !loaded) return;
    if (!canSave) {
      pushLog("error", "Saving is not supported for this file format yet.");
      return;
    }
    if (jsonError) {
      pushLog("error", "Fix the JSON errors before saving.");
      return;
    }
    setRunning(true);
    try {
      pushLog("info", "Saving config …");
      const result: string = await invoke("write_config", {
        configPath,
        configType,
        contentJson: jsonText,
        outPath: overwriteInput ? configPath : (outPath || null),
      });
      pushLog("success", `Saved → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  const handleConfigPathChange = useCallback((p: string) => {
    setConfigPath(p);
    setLoaded(false);
    setJsonText("");
    setConfigType("");
    setJsonError("");
    setCanSave(true);
    setLog([]);
  }, []);

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Config Editor</h2>
      <p className={styles.subtitle}>Read and edit .config .actor .conduit files</p>

      <div className={styles.panel}>
        <FilePickerInput
          label="Source file"
          value={configPath}
          onChange={handleConfigPathChange}
          mode="open"
          filters={CONFIG_FILTER}
        />
        <button className={styles.runBtn} onClick={loadConfig} disabled={running || !configPath}>
          {running ? "Loading…" : "Load Config"}
        </button>
      </div>

      {loaded && (
        <>
          <div className={styles.typeRow}>
            <span className={styles.typeLabel}>Type</span>
            <span className={styles.typeValue}>{configType}</span>
          </div>

          <div className={styles.editorWrap}>
            <div
              ref={borderRef}
              className={`${styles.editorBorder} ${jsonError ? styles.editorError : ""}`}
            >
              <CodeMirror
                value={jsonText}
                height={cmHeight}
                theme={vscodeDark}
                extensions={[json(), fontTheme]}
                onChange={onJsonChange}
                basicSetup={{ lineNumbers: true, foldGutter: true }}
              />
            </div>
            {jsonError && <span className={styles.errorMsg}>{jsonError}</span>}
          </div>

          <div className={styles.actions}>
            <FilePickerInput
              label="Output file (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={CONFIG_FILTER}
              placeholder="Leave blank — saves as _edited"
            />
            <div className={styles.actionRow}>
              <button
                className={styles.secondaryBtn}
                onClick={exportJson}
                disabled={running || !!jsonError}
                title="Export current editor content as .json"
              >
                Export JSON
              </button>
              <button
                className={styles.secondaryBtn}
                onClick={() => setSendToStager(overwriteInput ? configPath : (outPath || configPath))}
                disabled={running || !!jsonError || !canSave}
                title="Send output to a Stager project"
              >
                Send to Stager
              </button>
              <button
                className={styles.runBtn}
                onClick={saveConfig}
                disabled={running || !!jsonError || !canSave}
              >
                {running ? "Saving…" : "Save Config"}
              </button>
              <label className={styles.overwriteToggle}>
                <input
                  type="checkbox"
                  checked={overwriteInput}
                  onChange={(e) => setOverwriteInput(e.target.checked)}
                />
                <span>Overwrite input file</span>
              </label>
            </div>
          </div>
        </>
      )}

      <StatusLog entries={log} />

      {sendToStager && (
        <SendToStagerModal
          sourceFile={sendToStager}
          defaultTargetPath={assetPath ? `0/${assetPath}` : `0/${sendToStager.split(/[\\/]/).pop()}`}
          onClose={() => setSendToStager(null)}
          onSent={(proj) => {
            setSendToStager(null);
            pushLog("success", `Sent to project "${proj}"`);
          }}
        />
      )}
    </div>
  );
}
