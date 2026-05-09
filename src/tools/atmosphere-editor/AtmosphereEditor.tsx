import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./AtmosphereEditor.module.css";

const ATM_FILTER = [{ name: "Atmosphere", extensions: ["atmosphere"] }];

interface AtmosphereKnownValue {
  name: string;
  offset: number;
  value_type: string;
  value: string;
}

interface AtmosphereValueEdit {
  offset: number;
  value_type: string;
  value: string;
}

interface AtmosphereData {
  file_path: string;
  outer_magic: string;
  outer_size: number;
  dat1_magic: string;
  dat1_type_magic: string;
  dat1_total_size: number;
  sections: { tag: string; offset: number; size: number }[];
  known_values: AtmosphereKnownValue[];
  strings: string[];
  notes: string[];
}

export default function AtmosphereEditor() {
  const location = useLocation();
  const [atmospherePath, setAtmospherePath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [data, setData] = useState<AtmosphereData | null>(null);
  const [valueEdits, setValueEdits] = useState<Map<number, string>>(new Map());
  const [stringsText, setStringsText] = useState("");
  const [running, setRunning] = useState(false);
  const [log, setLog] = useState<LogEntry[]>([]);

  useEffect(() => {
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    if (filePath) setAtmospherePath(filePath);
  }, [location.state, location.search]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadAtmosphere() {
    if (!atmospherePath) {
      pushLog("error", "Select a .atmosphere file first.");
      return;
    }

    setRunning(true);
    setData(null);
    setValueEdits(new Map());
    setStringsText("");
    setLog([]);
    try {
      pushLog("info", `Reading ${atmospherePath} ...`);
      const result = await invoke<AtmosphereData>("read_atmosphere", { atmospherePath });
      setData(result);
      setStringsText(result.strings.join("\n"));
      pushLog(
        "success",
        `Loaded ${result.known_values.length} known value(s).`,
      );
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function updateValue(offset: number, value: string) {
    setValueEdits((prev) => {
      const next = new Map(prev);
      const original = data?.known_values.find((v) => v.offset === offset)?.value;
      if (original === value) {
        next.delete(offset);
      } else {
        next.set(offset, value);
      }
      return next;
    });
  }

  function isStringsDirty(): boolean {
    if (!data) return false;
    return stringsText !== data.strings.join("\n");
  }

  async function saveAtmosphere() {
    if (!data || !atmospherePath) return;

    const valueChanges: AtmosphereValueEdit[] = data.known_values
      .filter((v) => valueEdits.has(v.offset))
      .map((v) => ({
        offset: v.offset,
        value_type: v.value_type,
        value: valueEdits.get(v.offset) ?? v.value,
      }));

    const stringsPresent = data.sections.some((s) => s.tag.toUpperCase() === "72F28658");
    const stringsChanged = isStringsDirty();
    if (!stringsPresent && stringsChanged) {
      pushLog("error", "This file has no strings section (0x72F28658), so string editing cannot be saved.");
      return;
    }

    if (valueChanges.length === 0 && !stringsChanged) {
      pushLog("warning", "No changes to save.");
      return;
    }

    setRunning(true);
    try {
      pushLog("info", `Saving ${valueChanges.length} value edit(s)${stringsChanged ? " + strings" : ""} ...`);
      const stringsList = stringsChanged
        ? stringsText
          .split(/\r?\n/)
          .map((s) => s.trim())
          .filter((s) => s.length > 0)
        : null;

      const result = await invoke<string>("write_atmosphere", {
        atmospherePath,
        values: valueChanges,
        strings: stringsList,
        outPath: outPath || null,
      });

      pushLog("success", `Saved -> ${result}`);
      await loadAtmosphere();
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Atmosphere Editor</h2>
      <p className={styles.subtitle}>Inspect and edit known .atmosphere values and strings</p>

      <div className={styles.panel}>
        <FilePickerInput
          label="Source .atmosphere"
          value={atmospherePath}
          onChange={setAtmospherePath}
          mode="open"
          filters={ATM_FILTER}
        />
        <button className={styles.runBtn} onClick={loadAtmosphere} disabled={running}>
          {running ? "Loading..." : "Load Atmosphere"}
        </button>
      </div>

      {data && (
        <>          <section className={`${styles.sectionPane} ${styles.tablePane}`}>
          <h3>Known Values</h3>
          <table className={styles.table}>
            <thead>
              <tr>
                <th>Name</th>
                <th>Offset</th>
                <th>Type</th>
                <th>Value</th>
              </tr>
            </thead>
            <tbody>
              {data.known_values.map((v) => (
                <tr key={`${v.name}-${v.offset}`}>
                  <td>{v.name}</td>
                  <td>{v.offset}</td>
                  <td>{v.value_type}</td>
                  <td>
                    <input
                      className={styles.valueInput}
                      value={valueEdits.has(v.offset) ? valueEdits.get(v.offset)! : v.value}
                      onChange={(e) => updateValue(v.offset, e.target.value)}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>

          <section className={styles.sectionPane} style={{ flexShrink: 0 }}>
            <h3>Strings (0x72F28658)</h3>
            <p className={styles.helperText}>
              Stores sky texture paths</p>
            <textarea
              className={styles.stringsEditor}
              value={stringsText}
              onChange={(e) => setStringsText(e.target.value)}
              placeholder="One string per line"
              rows={1}
              disabled={!data.sections.some((s) => s.tag.toUpperCase() === "72F28658")}
            />
            {!data.sections.some((s) => s.tag.toUpperCase() === "72F28658") && (
              <p className={styles.emptyText}>No strings section (0x72F28658) in this file.</p>
            )}
          </section>

          <div className={styles.actions}>
            <FilePickerInput
              label="Output .atmosphere (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={ATM_FILTER}
              placeholder="Leave blank — saves as _edited"
            />
            <button
              className={styles.runBtn}
              onClick={saveAtmosphere}
              disabled={running || (valueEdits.size === 0 && !isStringsDirty())}
            >
              {running ? "Saving..." : `Save Changes (${valueEdits.size + (isStringsDirty() ? 1 : 0)})`}
            </button>
          </div>


        </>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
