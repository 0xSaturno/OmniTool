import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import { useSettings } from "../../contexts/SettingsContext";
import styles from "./AtmosphereEditor.module.css";

const ATM_FILTER = [{ name: "Atmosphere", extensions: ["atmosphere"] }];

const WEATHER_PRESETS = [
  { label: "Rain", source: "atm_zordoom" },
  { label: "Light rain", source: "atm_neo_nefarious_night_02" },
  { label: "Storm", source: "atm_zordoom_finale" },
  { label: "Downpour (dry)", source: "atm_ardolis_night_cine_2" },
  { label: "Snow", source: "atm_cordelion_surface_ruined_3" },
  { label: "Embers", source: "atm_blizarprime_day" },
];
const CLEAR_WEATHER = "__clear__";

type Tab = "values" | "weather" | "strings";
const TABS: { id: Tab; label: string }[] = [
  { id: "values", label: "Known Values" },
  { id: "weather", label: "Weather" },
  { id: "strings", label: "Strings" },
];
const WEATHER_REF_OFFSETS = [224, 248, 272, 512];
const WETNESS_OFFSET = 296;

function normalizeAssetPath(path: string): string {
  return path.trim().toLowerCase().replace(/\\/g, "/").replace(/\/+/g, "/").replace(/^\/|\/$/g, "");
}

interface AtmosphereKnownValue {
  name: string;
  offset: number;
  value_type: string;
  value: string;
  editable: boolean;
}

interface AtmosphereWeather {
  source_path: string;
  values: AtmosphereKnownValue[];
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
  shipped_path: string | null;
  dependencies: string[];
}

export default function AtmosphereEditor() {
  const location = useLocation();
  const { settings } = useSettings();
  const [atmospherePath, setAtmospherePath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [overwriteSource, setOverwriteSource] = useState(false);
  const [data, setData] = useState<AtmosphereData | null>(null);
  const [valueEdits, setValueEdits] = useState<Map<number, string>>(new Map());
  const [stringsText, setStringsText] = useState("");
  const [running, setRunning] = useState(false);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [valueFilter, setValueFilter] = useState("");
  const [atmospheres, setAtmospheres] = useState<string[]>([]);
  const [weatherSource, setWeatherSource] = useState("");
  const [tab, setTab] = useState<Tab>("values");

  const visibleValues = useMemo(() => {
    const q = valueFilter.trim().toLowerCase();
    if (!data || !q) return data?.known_values ?? [];
    return data.known_values.filter(
      (v) => v.name.toLowerCase().includes(q) || v.value.toLowerCase().includes(q) || String(v.offset) === q,
    );
  }, [data, valueFilter]);

  const weatherValues = useMemo(
    () => data?.known_values.filter((v) => v.name.startsWith("Weather.")) ?? [],
    [data],
  );

  // Weather refs the engine may not load: not among the open atmosphere's own dag dependencies.
  const undeclaredRefs = useMemo(() => {
    const warnings = new Map<number, string>();
    if (!data?.shipped_path) return warnings;
    const deps = new Set(data.dependencies);
    for (const offset of WEATHER_REF_OFFSETS) {
      const row = data.known_values.find((v) => v.offset === offset);
      const value = valueEdits.get(offset) ?? row?.value ?? "";
      if (row && value.includes(".") && !deps.has(normalizeAssetPath(value))) {
        warnings.set(offset, `${row.name} → ${value}`);
      }
    }
    return warnings;
  }, [data, valueEdits]);

  useEffect(() => {
    if (!settings.archivesDir) {
      setAtmospheres([]);
      return;
    }
    invoke<string[]>("list_atmospheres", { gameDir: settings.archivesDir })
      .then(setAtmospheres)
      .catch(() => setAtmospheres([]));
  }, [settings.archivesDir]);

  useEffect(() => {
    if (location.pathname !== "/tools/atmosphere-editor") return;
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    if (filePath) setAtmospherePath(filePath);
  }, [location.pathname, location.state, location.search]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadAtmosphere(keepLog = false) {
    if (!atmospherePath) {
      pushLog("error", "Select a .atmosphere file first.");
      return;
    }

    setRunning(true);
    setData(null);
    setValueEdits(new Map());
    setStringsText("");
    if (!keepLog) setLog([]);
    try {
      pushLog("info", `Reading ${atmospherePath} ...`);
      const result = await invoke<AtmosphereData>("read_atmosphere", {
        atmospherePath,
        gameDir: settings.archivesDir || null,
      });
      setData(result);
      setStringsText(result.strings.join("\n"));
      pushLog(
        "success",
        `Loaded ${result.known_values.length} known value(s).`,
      );
      for (const note of result.notes) pushLog("info", note);
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

  function applyEdits(values: { offset: number; value: string }[]): number {
    if (!data) return 0;
    const next = new Map(valueEdits);
    let changed = 0;
    for (const { offset, value } of values) {
      const original = data.known_values.find((v) => v.offset === offset)?.value;
      if (original === undefined) continue;
      if (original === value) {
        next.delete(offset);
      } else {
        next.set(offset, value);
        changed++;
      }
    }
    setValueEdits(next);
    return changed;
  }

  async function applyWeather() {
    if (!data || !weatherSource) return;
    if (weatherSource === CLEAR_WEATHER) {
      const changed = applyEdits([
        ...WEATHER_REF_OFFSETS.map((offset) => ({ offset, value: "<unset>" })),
        { offset: WETNESS_OFFSET, value: "0.000000" },
      ]);
      pushLog("info", `Cleared weather: ${changed} field(s) changed. Save to write them.`);
      return;
    }
    if (!settings.archivesDir) {
      pushLog("error", "Set the game folder in Settings to copy weather from a shipped atmosphere.");
      return;
    }
    setRunning(true);
    try {
      const weather = await invoke<AtmosphereWeather>("read_atmosphere_weather", {
        source: weatherSource,
        gameDir: settings.archivesDir,
      });
      const changed = applyEdits(weather.values);
      pushLog("info", `Copied weather from ${weather.source_path}: ${changed} field(s) changed. Save to write them.`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
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
        outPath: overwriteSource ? atmospherePath : (outPath || null),
        gameDir: settings.archivesDir || null,
      });

      pushLog("success", `Saved -> ${result}`);
      for (const ref of undeclaredRefs.values()) {
        pushLog(
          "warning",
          `${ref} is not a dag dependency of ${data.shipped_path}; the game may not load it.`,
        );
      }
      await loadAtmosphere(true);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function renderValueTable(rows: AtmosphereKnownValue[]) {
    return (
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
          {rows.map((v) => (
            <tr key={`${v.name}-${v.offset}`}>
              <td>{v.name}</td>
              <td>{v.offset}</td>
              <td>{v.value_type}</td>
              <td>
                <input
                  className={`${styles.valueInput} ${undeclaredRefs.has(v.offset) ? styles.valueInputWarn : ""}`}
                  value={valueEdits.has(v.offset) ? valueEdits.get(v.offset)! : v.value}
                  onChange={(e) => updateValue(v.offset, e.target.value)}
                  readOnly={!v.editable}
                  title={
                    undeclaredRefs.has(v.offset)
                      ? "Not a dag dependency of this atmosphere; the game may not load it."
                      : v.value_type === "asset" && v.editable
                        ? `Game path of a ${v.offset === 512 ? ".material" : ".texture"}; leave empty to clear.`
                        : v.value_type === "asset"
                          ? "Read-only. Editing the matching path in Strings updates this id."
                          : v.value_type === "path"
                            ? "Read-only. Path stored in the file's string pool."
                            : undefined
                  }
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    );
  }

  const hasStrings = data?.sections.some((s) => s.tag.toUpperCase() === "72F28658") ?? false;

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>Atmosphere Editor</h2>
        <span className={styles.subtitle}>Inspect and edit known .atmosphere values</span>
      </div>

      <div className={styles.panel}>
        <FilePickerInput
          label="Source .atmosphere"
          value={atmospherePath}
          onChange={setAtmospherePath}
          mode="open"
          filters={ATM_FILTER}
        />
        <button className={styles.runBtn} onClick={() => loadAtmosphere()} disabled={running || !atmospherePath}>
          {running ? "Loading..." : "Load Atmosphere"}
        </button>
      </div>

      {data && (
        <>
          <div className={styles.tabs}>
            {TABS.map((t) => (
              <button
                key={t.id}
                className={`${styles.tab} ${tab === t.id ? styles.active : ""}`}
                onClick={() => setTab(t.id)}
              >
                {t.label}
                {t.id === "weather" && undeclaredRefs.size > 0 && <span className={styles.tabWarn}>●</span>}
              </button>
            ))}
          </div>

          {tab === "values" && (
            <section className={`${styles.sectionPane} ${styles.tablePane}`}>
              <input
                className={styles.valueInput}
                style={{ marginBottom: "0.5rem" }}
                placeholder="Filter by name, value or offset (e.g. Fog, Clouds, 1120)"
                value={valueFilter}
                onChange={(e) => setValueFilter(e.target.value)}
              />
              {renderValueTable(visibleValues)}
            </section>
          )}

          {tab === "weather" && (
            <section className={`${styles.sectionPane} ${styles.tablePane}`}>
              <p className={styles.helperText}>
                Copies the wet-surface, particle and wind fields from a shipped atmosphere as edits; review them
                below, then save.
                {data.shipped_path && ` Matched to ${data.shipped_path}.`}
              </p>
              <div className={styles.weatherRow}>
                <select
                  className={styles.weatherSelect}
                  value={weatherSource}
                  onChange={(e) => setWeatherSource(e.target.value)}
                >
                  <option value="">Choose weather…</option>
                  <optgroup label="Presets">
                    {WEATHER_PRESETS.map((p) => (
                      <option key={p.source} value={p.source}>
                        {p.label} ({p.source})
                      </option>
                    ))}
                    <option value={CLEAR_WEATHER}>Clear</option>
                  </optgroup>
                  {atmospheres.length > 0 && (
                    <optgroup label="Copy weather from">
                      {atmospheres.map((path) => (
                        <option key={path} value={path}>
                          {path}
                        </option>
                      ))}
                    </optgroup>
                  )}
                </select>
                <button
                  className={styles.runBtn}
                  onClick={applyWeather}
                  disabled={running || !weatherSource}
                >
                  Apply
                </button>
              </div>
              {undeclaredRefs.size > 0 && (
                <ul className={styles.warnList}>
                  {[...undeclaredRefs.values()].map((ref) => (
                    <li key={ref}>
                      {ref} is not a dag dependency of this atmosphere; the game may not load it unless something
                      else already has.
                    </li>
                  ))}
                </ul>
              )}
              <div className={styles.weatherTable}>{renderValueTable(weatherValues)}</div>
            </section>
          )}

          {tab === "strings" && (
            <section className={`${styles.sectionPane} ${styles.tablePane} ${styles.stringsPane}`}>
              <p className={styles.helperText}>Section 0x72F28658 — stores sky texture paths, one per line.</p>
              <textarea
                className={styles.stringsEditor}
                value={stringsText}
                onChange={(e) => setStringsText(e.target.value)}
                placeholder="One string per line"
                disabled={!hasStrings}
              />
              {!hasStrings && <p className={styles.emptyText}>No strings section (0x72F28658) in this file.</p>}
            </section>
          )}

          <div className={styles.actions}>
            <FilePickerInput
              label="Output .atmosphere (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={ATM_FILTER}
              placeholder="Leave blank — saves as _edited"
            />
            <div className={styles.saveRow}>
              <button
                className={styles.runBtn}
                onClick={saveAtmosphere}
                disabled={running || (valueEdits.size === 0 && !isStringsDirty())}
              >
                {running ? "Saving..." : `Save Changes (${valueEdits.size + (isStringsDirty() ? 1 : 0)})`}
              </button>
              <label className={styles.overwriteToggle}>
                <input
                  type="checkbox"
                  checked={overwriteSource}
                  onChange={(e) => setOverwriteSource(e.target.checked)}
                />
                <span>Overwrite source atmosphere</span>
              </label>
            </div>
          </div>
        </>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
