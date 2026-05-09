import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./ZoneLightBinModule.module.css";

const ZLB_FILTER = [{ name: "ZoneLightBin", extensions: ["zonelightbin"] }];

interface ZlbSectionInfo {
  tag: string;
  offset: number;
  declared_size: number;
  available_size: number;
  crc32: number;
  truncated: boolean;
}

interface ZlbDat1Info {
  magic: string;
  type_magic: string;
  declared_total_size: number;
  available_size: number;
  start_offset: number;
  sections: ZlbSectionInfo[];
  truncated: boolean;
}

interface ZoneLightBinData {
  file_path: string;
  file_size: number;
  wrapper_magic: string;
  wrapper_size: number;
  primary: ZlbDat1Info;
  bridge_bytes_hex: string;
  bridge_offset: number;
  bridge_length: number;
  secondary: ZlbDat1Info | null;
  trailing_after_secondary: number;
  notes: string[];
}

interface ZlbCopyOptions {
  primary_27204b67: boolean;
  primary_101a2196: boolean;
  secondary_13f4af3b: boolean;
  secondary_c72a514c: boolean;
}

interface ZlbDiffSection {
  tag: string;
  layer: string;
  base_size: number | null;
  reference_size: number | null;
  equal: boolean;
  byte_diffs: number | null;
}

interface ZlbDiffResult {
  base_path: string;
  reference_path: string;
  base_file_size: number;
  reference_file_size: number;
  sections: ZlbDiffSection[];
  notes: string[];
}

const DEFAULT_OPTIONS: ZlbCopyOptions = {
  primary_27204b67: false,
  primary_101a2196: false,
  secondary_13f4af3b: false,
  secondary_c72a514c: false,
};

function formatHex(n: number): string {
  return `0x${n.toString(16).toUpperCase().padStart(8, "0")}`;
}

export default function ZoneLightBinModule() {
  const location = useLocation();
  const [basePath, setBasePath] = useState("");
  const [referencePath, setReferencePath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [data, setData] = useState<ZoneLightBinData | null>(null);
  const [options, setOptions] = useState<ZlbCopyOptions>({ ...DEFAULT_OPTIONS });
  const [diff, setDiff] = useState<ZlbDiffResult | null>(null);
  const [running, setRunning] = useState(false);
  const [log, setLog] = useState<LogEntry[]>([]);

  useEffect(() => {
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    if (filePath) setBasePath(filePath);
  }, [location.state, location.search]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadBase() {
    if (!basePath) {
      pushLog("error", "Select a base .zonelightbin file first.");
      return;
    }
    setRunning(true);
    setData(null);
    setDiff(null);
    try {
      pushLog("info", `Reading ${basePath} ...`);
      const result = await invoke<ZoneLightBinData>("read_zonelightbin", {
        zlbPath: basePath,
      });
      setData(result);
      pushLog(
        "success",
        `Loaded: primary ${result.primary.sections.length} section(s)` +
        (result.secondary
          ? `, secondary ${result.secondary.sections.length} section(s)`
          : ", no secondary DAT1"),
      );
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function toggleOption(key: keyof ZlbCopyOptions) {
    setOptions((prev) => ({ ...prev, [key]: !prev[key] }));
  }

  function selectAll(value: boolean) {
    setOptions({
      primary_27204b67: value,
      primary_101a2196: value,
      secondary_13f4af3b: value,
      secondary_c72a514c: value,
    });
  }

  const anyOption =
    options.primary_27204b67 ||
    options.primary_101a2196 ||
    options.secondary_13f4af3b ||
    options.secondary_c72a514c;

  async function saveSections() {
    if (!basePath || !referencePath) {
      pushLog("error", "Select both base and reference .zonelightbin files.");
      return;
    }
    if (!anyOption) {
      pushLog("warning", "No copy options selected.");
      return;
    }
    setRunning(true);
    try {
      pushLog(
        "info",
        `Copying selected section(s) from reference -> base ...`,
      );
      const result = await invoke<string>("write_zonelightbin_sections", {
        basePath,
        referencePath,
        options,
        outPath: outPath || null,
      });
      pushLog("success", `Saved -> ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function runDiff() {
    if (!basePath || !referencePath) {
      pushLog("error", "Select both base and reference .zonelightbin files.");
      return;
    }
    setRunning(true);
    setDiff(null);
    try {
      pushLog("info", `Diffing base vs reference ...`);
      const result = await invoke<ZlbDiffResult>("diff_zonelightbin", {
        basePath,
        referencePath,
      });
      setDiff(result);
      const changed = result.sections.filter((s) => !s.equal).length;
      pushLog(
        changed > 0 ? "warning" : "success",
        `Diff complete: ${changed} of ${result.sections.length} known sections differ.`,
      );
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function applyFromDiff(layer: "primary" | "secondary", tag: string) {
    const upper = tag.toUpperCase();
    setOptions((prev) => {
      const next = { ...prev };
      if (layer === "primary" && upper === "27204B67") next.primary_27204b67 = true;
      else if (layer === "primary" && upper === "101A2196") next.primary_101a2196 = true;
      else if (layer === "secondary" && upper === "13F4AF3B") next.secondary_13f4af3b = true;
      else if (layer === "secondary" && upper === "C72A514C") next.secondary_c72a514c = true;
      return next;
    });
    pushLog("info", `Marked ${layer} 0x${upper} for copy from reference.`);
  }

  function diffBadge(s: ZlbDiffSection) {
    if (s.base_size == null || s.reference_size == null) {
      return <span className={`${styles.diffBadge} ${styles.diffMissing}`}>missing</span>;
    }
    if (s.equal) {
      return <span className={`${styles.diffBadge} ${styles.diffEqual}`}>equal</span>;
    }
    return <span className={`${styles.diffBadge} ${styles.diffChanged}`}>changed</span>;
  }

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>ZoneLightBin Inspector</h2>
      <p className={styles.subtitle}>
        Inspect, diff, and section-copy <code>.zonelightbin</code> assets
        (light tile grids)
      </p>

      <div className={styles.panel}>
        <FilePickerInput
          label="Base .zonelightbin (the file you want to modify / inspect)"
          value={basePath}
          onChange={setBasePath}
          mode="open"
          filters={ZLB_FILTER}
        />
        <FilePickerInput
          label="Reference .zonelightbin (source of section copies / diff target)"
          value={referencePath}
          onChange={setReferencePath}
          mode="open"
          filters={ZLB_FILTER}
        />
        <div className={styles.actionsRow}>
          <button className={styles.runBtn} onClick={loadBase} disabled={running}>
            {running ? "Working..." : "Load / Inspect Base"}
          </button>
          <button
            className={styles.secondaryBtn}
            onClick={runDiff}
            disabled={running || !basePath || !referencePath}
          >
            Diff Base vs Reference
          </button>
        </div>
      </div>

      {data && (
        <>
          <div className={styles.metaGrid}>
            <div className={styles.metaCard}>
              <span>Wrapper Magic</span>
              <strong>{data.wrapper_magic}</strong>
            </div>
            <div className={styles.metaCard}>
              <span>Wrapper Size</span>
              <strong>{data.wrapper_size.toLocaleString()}</strong>
            </div>
            <div className={styles.metaCard}>
              <span>File Size</span>
              <strong>{data.file_size.toLocaleString()}</strong>
            </div>
            <div className={styles.metaCard}>
              <span>Primary DAT1</span>
              <strong>
                {data.primary.magic} / {data.primary.type_magic}
              </strong>
            </div>
            <div className={styles.metaCard}>
              <span>Primary Total</span>
              <strong>
                {data.primary.declared_total_size.toLocaleString()}
                {data.primary.truncated &&
                  ` (avail ${data.primary.available_size.toLocaleString()})`}
              </strong>
            </div>
            <div className={styles.metaCard}>
              <span>Secondary DAT1</span>
              <strong>
                {data.secondary
                  ? `${data.secondary.magic} / ${data.secondary.type_magic}`
                  : "—"}
              </strong>
            </div>
            <div className={styles.metaCard}>
              <span>Bridge Bytes</span>
              <strong>{data.bridge_length}</strong>
            </div>
            <div className={styles.metaCard}>
              <span>Trailing After Sec.</span>
              <strong>{data.trailing_after_secondary.toLocaleString()}</strong>
            </div>
          </div>

          <section className={styles.sectionPane}>
            <div className={styles.layerHeader}>
              <h3>Primary DAT1 sections</h3>
              <span className={styles.layerSubLabel}>
                start @ {data.primary.start_offset}
              </span>
            </div>
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>Tag</th>
                  <th>Offset</th>
                  <th>Declared Size</th>
                  <th>Available</th>
                  <th>CRC32</th>
                </tr>
              </thead>
              <tbody>
                {data.primary.sections.map((s) => (
                  <tr key={`p-${s.tag}`}>
                    <td>0x{s.tag}</td>
                    <td>{s.offset.toLocaleString()}</td>
                    <td>{s.declared_size.toLocaleString()}</td>
                    <td>
                      {s.available_size.toLocaleString()}
                      {s.truncated && (
                        <span
                          className={`${styles.diffBadge} ${styles.diffMissing}`}
                          style={{ marginLeft: "0.4rem" }}
                        >
                          truncated
                        </span>
                      )}
                    </td>
                    <td>{formatHex(s.crc32)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>

          {data.secondary && (
            <section className={styles.sectionPane}>
              <div className={styles.layerHeader}>
                <h3>Secondary DAT1 sections</h3>
                <span className={styles.layerSubLabel}>
                  start @ {data.secondary.start_offset} • declared{" "}
                  {data.secondary.declared_total_size.toLocaleString()}
                  {data.secondary.truncated &&
                    ` • avail ${data.secondary.available_size.toLocaleString()}`}
                </span>
              </div>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th>Tag</th>
                    <th>Offset</th>
                    <th>Declared Size</th>
                    <th>Available</th>
                    <th>CRC32</th>
                  </tr>
                </thead>
                <tbody>
                  {data.secondary.sections.map((s) => (
                    <tr key={`s-${s.tag}`}>
                      <td>0x{s.tag}</td>
                      <td>{s.offset.toLocaleString()}</td>
                      <td>{s.declared_size.toLocaleString()}</td>
                      <td>
                        {s.available_size.toLocaleString()}
                        {s.truncated && (
                          <span
                            className={`${styles.diffBadge} ${styles.diffMissing}`}
                            style={{ marginLeft: "0.4rem" }}
                          >
                            truncated
                          </span>
                        )}
                      </td>
                      <td>{formatHex(s.crc32)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </section>
          )}

          <section className={styles.sectionPane}>
            <h3>Bridge bytes (between primary and secondary DAT1)</h3>
            <p className={styles.helperText}>
              Preserved verbatim on save. Samples typically show 2 bytes here.
            </p>
            <div className={styles.bridgeBox}>
              {data.bridge_bytes_hex || "(none)"}
            </div>
          </section>

          <section className={styles.sectionPane}>
            <h3>Section copy (Phase 2)</h3>
            <p className={styles.helperText}>
              Copies the selected section bytes from the <strong>reference</strong>{" "}
              file into the <strong>base</strong> file. Untouched sections, the
              wrapper header, the bridge bytes, and any trailing data are
              preserved exactly. The wrapper size field is updated to match the
              re-serialized primary DAT1.
            </p>
            <div className={styles.optionList}>
              <label className={styles.optionRow}>
                <input
                  type="checkbox"
                  checked={options.primary_27204b67}
                  onChange={() => toggleOption("primary_27204b67")}
                />
                Primary <code>0x27204B67</code> (large u32 array)
              </label>
              <label className={styles.optionRow}>
                <input
                  type="checkbox"
                  checked={options.primary_101a2196}
                  onChange={() => toggleOption("primary_101a2196")}
                />
                Primary <code>0x101A2196</code> (8 bytes)
              </label>
              <label className={styles.optionRow}>
                <input
                  type="checkbox"
                  checked={options.secondary_13f4af3b}
                  onChange={() => toggleOption("secondary_13f4af3b")}
                  disabled={!data.secondary}
                />
                Secondary <code>0x13F4AF3B</code> (dominant lighting payload)
              </label>
              <label className={styles.optionRow}>
                <input
                  type="checkbox"
                  checked={options.secondary_c72a514c}
                  onChange={() => toggleOption("secondary_c72a514c")}
                  disabled={!data.secondary}
                />
                Secondary <code>0xC72A514C</code>
              </label>
            </div>

            <div className={styles.actionsRow}>
              <button
                className={styles.secondaryBtn}
                onClick={() => selectAll(true)}
                disabled={running}
              >
                Select all
              </button>
              <button
                className={styles.secondaryBtn}
                onClick={() => selectAll(false)}
                disabled={running}
              >
                Clear
              </button>
              <button
                className={styles.secondaryBtn}
                onClick={() => {
                  setOptions({
                    primary_27204b67: true,
                    primary_101a2196: true,
                    secondary_13f4af3b: !!data.secondary,
                    secondary_c72a514c: !!data.secondary,
                  });
                  pushLog(
                    "info",
                    "Selected all sections (one-click full transfer from reference).",
                  );
                }}
                disabled={running}
              >
                One-click full transfer
              </button>
            </div>

            <FilePickerInput
              label="Output .zonelightbin (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={ZLB_FILTER}
              placeholder="Leave blank — saves as _edited next to base"
            />
            <button
              className={styles.runBtn}
              onClick={saveSections}
              disabled={running || !anyOption || !referencePath}
            >
              {running
                ? "Saving..."
                : `Apply Section Copy (${[
                  options.primary_27204b67,
                  options.primary_101a2196,
                  options.secondary_13f4af3b,
                  options.secondary_c72a514c,
                ].filter(Boolean).length})`}
            </button>
          </section>

          {data.notes.length > 0 && (
            <section className={styles.sectionPane}>
              <h3>Notes</h3>
              <ul className={styles.noteList}>
                {data.notes.map((n, i) => (
                  <li key={`${n}-${i}`}>{n}</li>
                ))}
              </ul>
            </section>
          )}
        </>
      )}

      {diff && (
        <section className={styles.sectionPane}>
          <div className={styles.layerHeader}>
            <h3>Diff: base vs reference</h3>
            <span className={styles.layerSubLabel}>
              base {diff.base_file_size.toLocaleString()} B • reference{" "}
              {diff.reference_file_size.toLocaleString()} B
            </span>
          </div>
          <table className={styles.table}>
            <thead>
              <tr>
                <th>Layer</th>
                <th>Tag</th>
                <th>Base size</th>
                <th>Ref size</th>
                <th>Status</th>
                <th>Byte diffs</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {diff.sections.map((s) => (
                <tr key={`${s.layer}-${s.tag}`}>
                  <td>{s.layer}</td>
                  <td>0x{s.tag}</td>
                  <td>{s.base_size != null ? s.base_size.toLocaleString() : "—"}</td>
                  <td>
                    {s.reference_size != null
                      ? s.reference_size.toLocaleString()
                      : "—"}
                  </td>
                  <td>{diffBadge(s)}</td>
                  <td>
                    {s.byte_diffs != null
                      ? s.byte_diffs.toLocaleString()
                      : s.equal
                        ? "0"
                        : "size differs"}
                  </td>
                  <td>
                    {!s.equal &&
                      s.base_size != null &&
                      s.reference_size != null && (
                        <button
                          className={styles.secondaryBtn}
                          onClick={() =>
                            applyFromDiff(
                              s.layer as "primary" | "secondary",
                              s.tag,
                            )
                          }
                        >
                          Mark for copy
                        </button>
                      )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {diff.notes.length > 0 && (
            <ul className={styles.noteList}>
              {diff.notes.map((n, i) => (
                <li key={`${n}-${i}`}>{n}</li>
              ))}
            </ul>
          )}
        </section>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
