import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import { useSettings } from "../../contexts/SettingsContext";
import FilePickerInput from "../../components/shared/FilePickerInput";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import { deriveStagerTarget } from "../../utils/stagerTarget";
import styles from "./MaterialEditor.module.css";

const MATERIAL_FILTER = [{ name: "Insomniac Material", extensions: ["material"] }];

interface MaterialHeader {
  unk00: number;
  unk04: number;
  flags: number;
  audio_material_hash: number;
  av_material_hash: number;
  unk14: number;
  unk18: number;
  unk1c: number;
  unk20: number;
  unk24: number;
}

interface SamplerView {
  name_hash: number;
  name: string;
  path: string;
  slot_index: number | null;
  default_path: string | null;
  in_template: boolean;
  overridden: boolean;
}

interface ConstantView {
  name_hash: number;
  name: string;
  values: number[];
  default_values: number[] | null;
  in_template: boolean;
  overridden: boolean;
}

interface MaterialDocument {
  path: string;
  template_path: string | null;
  template_loaded: boolean;
  template_note: string | null;
  header: MaterialHeader;
  av_material_name: string | null;
  audio_material_name: string | null;
  samplers: SamplerView[];
  constants: ConstantView[];
  has_fur_section: boolean;
  section_tags: string[];
}

type Tab = "textures" | "constants" | "header";

// Bits seen in shipped materials; labels marked (?) are inferred, not verified in-engine.
const FLAG_BITS: { bit: number; label: string }[] = [
  { bit: 0, label: "Bit 0" },
  { bit: 2, label: "Bit 2" },
  { bit: 4, label: "Bit 4" },
  { bit: 7, label: "Bit 7" },
  { bit: 8, label: "Bit 8" },
  { bit: 11, label: "Bit 11 (always set)" },
  { bit: 12, label: "Bit 12" },
  { bit: 14, label: "Bit 14 — Fur (?)" },
  { bit: 15, label: "Bit 15 — Alpha blended (?)" },
  { bit: 18, label: "Bit 18" },
  { bit: 21, label: "Bit 21 — Eye/refractive (?)" },
];

function hex(n: number): string {
  return `0x${(n >>> 0).toString(16).toUpperCase().padStart(8, "0")}`;
}

function sameValues(a: number[], b: number[] | null): boolean {
  if (!b || a.length !== b.length) return false;
  return a.every((v, i) => Math.abs(v - b[i]) < 1e-9);
}

function toHexColor(values: number[]): string {
  const clamp = (v: number) => Math.max(0, Math.min(255, Math.round(v * 255)));
  const [r, g, b] = values;
  return `#${[r, g, b].map((v) => clamp(v).toString(16).padStart(2, "0")).join("")}`;
}

function fromHexColor(css: string, length: number, previous: number[]): number[] {
  const r = parseInt(css.slice(1, 3), 16) / 255;
  const g = parseInt(css.slice(3, 5), 16) / 255;
  const b = parseInt(css.slice(5, 7), 16) / 255;
  const out = [r, g, b];
  if (length > 3) out.push(previous[3] ?? 1);
  return out;
}

const ZOOM_SIZE = 280;
const ZOOM_GAP = 18;

// Keep the floating preview inside the viewport, flipping sides near an edge.
function clampZoom(x: number, y: number): { x: number; y: number } {
  const w = ZOOM_SIZE + 16;
  const h = ZOOM_SIZE + 40;
  const left = x + ZOOM_GAP + w > window.innerWidth ? x - ZOOM_GAP - w : x + ZOOM_GAP;
  const top = Math.min(Math.max(y - h / 2, 8), Math.max(8, window.innerHeight - h - 8));
  return { x: Math.max(8, left), y: top };
}

function isColor(c: ConstantView): boolean {
  return (c.values.length === 3 || c.values.length === 4) && /colou?r|tint/i.test(c.name);
}

export default function MaterialEditor() {
  const location = useLocation();
  const { settings } = useSettings();

  const [materialPath, setMaterialPath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [assetPath, setAssetPath] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("textures");
  const [sendToStager, setSendToStager] = useState<string | null>(null);

  const [doc, setDoc] = useState<MaterialDocument | null>(null);
  const [samplers, setSamplers] = useState<SamplerView[]>([]);
  const [constants, setConstants] = useState<ConstantView[]>([]);
  const [header, setHeader] = useState<MaterialHeader | null>(null);
  const [previews, setPreviews] = useState<Record<string, string>>({});
  const [zoom, setZoom] = useState<{ src: string; label: string; x: number; y: number } | null>(
    null,
  );

  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    if (location.pathname !== "/tools/material-editor") return;
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string; assetPath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    const incoming = s?.assetPath ?? params.get("assetPath") ?? undefined;
    if (filePath) setMaterialPath(filePath);
    if (incoming) setAssetPath(incoming);
  }, [location.pathname, location.state, location.search]);

  const pushLog = useCallback((type: LogEntry["type"], message: string) => {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }, []);

  async function load() {
    if (!materialPath) {
      pushLog("error", "Select a .material file first.");
      return;
    }
    setRunning(true);
    setLog([]);
    setPreviews({});
    try {
      const archivesDir = settings.archivesDir;
      const result: MaterialDocument = await invoke("read_material", {
        materialPath,
        tocPath: archivesDir ? `${archivesDir}\\toc` : null,
        archivesDir: archivesDir || null,
        sourceMode: "live",
      });
      setDoc(result);
      setSamplers(result.samplers);
      setConstants(result.constants);
      setHeader(result.header);
      pushLog(
        "success",
        `Loaded ${result.samplers.length} texture slot(s), ${result.constants.length} constant(s).`,
      );
      if (result.template_path) {
        if (result.template_loaded) {
          pushLog("info", `Template: ${result.template_path}`);
        } else {
          pushLog(
            "warning",
            `Template ${result.template_path} not loaded — ${result.template_note}`,
          );
        }
      }
      if (result.has_fur_section) {
        pushLog("info", "Material has a Fur Info section — preserved untouched on save.");
      }
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function save() {
    if (!materialPath || !header) return;
    setRunning(true);
    try {
      const result: string = await invoke("save_material", {
        materialPath,
        header,
        samplers: samplers
          .filter((s) => s.overridden)
          .map((s) => ({ name_hash: s.name_hash, path: s.path })),
        constants: constants
          .filter((c) => c.overridden)
          .map((c) => ({ name_hash: c.name_hash, values: c.values })),
        outPath: outPath || null,
      });
      pushLog("success", `Saved → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function loadPreview(path: string) {
    if (previews[path] || !settings.archivesDir) return;
    try {
      const assetId: string = await invoke("compute_crc64", { input: path });
      const temp: string = await invoke("extract_to_temp", {
        tocPath: `${settings.archivesDir}\\toc`,
        assetId,
        archivesDir: settings.archivesDir,
        filename: path,
        sourceMode: "live",
      });
      const base64: string = await invoke("tauri_get_texture_preview", { path: temp });
      setPreviews((prev) => ({ ...prev, [path]: `data:image/png;base64,${base64}` }));
    } catch (e) {
      pushLog("error", `Preview failed for ${path}: ${e}`);
    }
  }

  function updateSampler(nameHash: number, path: string) {
    setSamplers((prev) =>
      prev.map((s) =>
        s.name_hash === nameHash
          ? { ...s, path, overridden: s.default_path === null || path !== s.default_path }
          : s,
      ),
    );
  }

  function resetSampler(s: SamplerView) {
    if (s.default_path === null) return;
    setSamplers((prev) =>
      prev.map((x) =>
        x.name_hash === s.name_hash ? { ...x, path: s.default_path!, overridden: false } : x,
      ),
    );
  }

  function updateConstant(nameHash: number, index: number, value: number) {
    setConstants((prev) =>
      prev.map((c) => {
        if (c.name_hash !== nameHash) return c;
        const values = c.values.slice();
        values[index] = value;
        return { ...c, values, overridden: !sameValues(values, c.default_values) };
      }),
    );
  }

  function setConstantValues(nameHash: number, values: number[]) {
    setConstants((prev) =>
      prev.map((c) =>
        c.name_hash === nameHash
          ? { ...c, values, overridden: !sameValues(values, c.default_values) }
          : c,
      ),
    );
  }

  function resetConstant(c: ConstantView) {
    if (!c.default_values) return;
    setConstantValues(c.name_hash, c.default_values.slice());
    setConstants((prev) =>
      prev.map((x) => (x.name_hash === c.name_hash ? { ...x, overridden: false } : x)),
    );
  }

  function toggleFlagBit(bit: number) {
    setHeader((prev) => (prev ? { ...prev, flags: prev.flags ^ (1 << bit) } : prev));
  }

  const dirty =
    doc !== null &&
    (JSON.stringify(samplers) !== JSON.stringify(doc.samplers) ||
      JSON.stringify(constants) !== JSON.stringify(doc.constants) ||
      JSON.stringify(header) !== JSON.stringify(doc.header));

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Material Editor</h2>
      <p className={styles.subtitle}>
        Edit texture slots, shader constants and render flags inside .material files
      </p>

      <div className={styles.panel}>
        <FilePickerInput
          label="Source .material"
          value={materialPath}
          onChange={setMaterialPath}
          mode="open"
          filters={MATERIAL_FILTER}
        />
        <button className={styles.runBtn} onClick={load} disabled={running}>
          {running ? "Loading…" : "Load Material"}
        </button>
      </div>

      {doc && (
        <>
          <div className={styles.templateBar}>
            <span className={styles.dimmed}>Template</span>
            <code className={styles.templatePath}>{doc.template_path ?? "—"}</code>
            <span className={doc.template_loaded ? styles.okBadge : styles.warnBadge}>
              {doc.template_loaded ? "slots resolved" : "slots unresolved"}
            </span>
            {doc.has_fur_section && <span className={styles.badge}>fur</span>}
          </div>

          <div className={styles.tabs}>
            {(["textures", "constants", "header"] as Tab[]).map((t) => (
              <button
                key={t}
                className={`${styles.tab} ${tab === t ? styles.active : ""}`}
                onClick={() => setTab(t)}
              >
                {t === "textures"
                  ? `Textures (${samplers.length})`
                  : t === "constants"
                    ? `Constants (${constants.length})`
                    : "Header"}
              </button>
            ))}
          </div>

          {tab === "textures" && (
            <div className={styles.tableWrap}>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th className={styles.th}>Slot</th>
                    <th className={styles.th}>Name</th>
                    <th className={`${styles.th} ${styles.thPath}`}>Texture</th>
                    <th className={styles.th}>State</th>
                    <th className={styles.th} />
                  </tr>
                </thead>
                <tbody>
                  {samplers.map((s) => (
                    <tr key={s.name_hash} className={s.overridden ? styles.changed : ""}>
                      <td className={styles.td}>{s.slot_index ?? "—"}</td>
                      <td className={styles.td} title={hex(s.name_hash)}>
                        {s.name}
                      </td>
                      <td className={styles.td}>
                        <div className={styles.pathCell}>
                          {previews[s.path] && (
                            <img
                              className={styles.thumb}
                              src={previews[s.path]}
                              alt=""
                              onMouseEnter={(e) =>
                                setZoom({
                                  src: previews[s.path],
                                  label: s.path.split("/").pop() ?? s.path,
                                  ...clampZoom(e.clientX, e.clientY),
                                })
                              }
                              onMouseMove={(e) =>
                                setZoom((z) => (z ? { ...z, ...clampZoom(e.clientX, e.clientY) } : z))
                              }
                              onMouseLeave={() => setZoom(null)}
                            />
                          )}
                          <input
                            className={styles.pathInput}
                            value={s.path}
                            onChange={(e) => updateSampler(s.name_hash, e.target.value)}
                          />
                        </div>
                      </td>
                      <td className={`${styles.td} ${styles.dimmed}`}>
                        {s.overridden ? "override" : "template default"}
                      </td>
                      <td className={styles.td}>
                        <button
                          className={styles.miniBtn}
                          onClick={() => loadPreview(s.path)}
                          disabled={!settings.archivesDir}
                          title="Extract from archives and preview"
                        >
                          preview
                        </button>
                        <button
                          className={styles.miniBtn}
                          onClick={() => resetSampler(s)}
                          disabled={s.default_path === null || !s.overridden}
                          title="Restore the material graph default"
                        >
                          reset
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {tab === "constants" && (
            <div className={styles.tableWrap}>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th className={styles.th}>Name</th>
                    <th className={`${styles.th} ${styles.thPath}`}>Value</th>
                    <th className={styles.th}>Default</th>
                    <th className={styles.th}>State</th>
                    <th className={styles.th} />
                  </tr>
                </thead>
                <tbody>
                  {constants.map((c) => (
                    <tr key={c.name_hash} className={c.overridden ? styles.changed : ""}>
                      <td className={styles.td} title={hex(c.name_hash)}>
                        {c.name}
                      </td>
                      <td className={styles.td}>
                        <div className={styles.valueCell}>
                          {isColor(c) && (
                            <input
                              type="color"
                              className={styles.colorInput}
                              value={toHexColor(c.values)}
                              onChange={(e) =>
                                setConstantValues(
                                  c.name_hash,
                                  fromHexColor(e.target.value, c.values.length, c.values),
                                )
                              }
                            />
                          )}
                          {c.values.map((v, i) => (
                            <input
                              key={i}
                              type="number"
                              step="0.01"
                              className={styles.numInput}
                              value={v}
                              onChange={(e) =>
                                updateConstant(c.name_hash, i, Number(e.target.value))
                              }
                            />
                          ))}
                        </div>
                      </td>
                      <td className={`${styles.td} ${styles.dimmed}`}>
                        {c.default_values ? c.default_values.map((v) => v.toFixed(3)).join(", ") : "—"}
                      </td>
                      <td className={`${styles.td} ${styles.dimmed}`}>
                        {c.overridden ? "override" : "template default"}
                      </td>
                      <td className={styles.td}>
                        <button
                          className={styles.miniBtn}
                          onClick={() => resetConstant(c)}
                          disabled={!c.default_values || !c.overridden}
                        >
                          reset
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {tab === "header" && header && (
            <div className={styles.headerPanel}>
              <div className={styles.field}>
                <label className={styles.fieldLabel}>Render flags</label>
                <input
                  className={styles.hexInput}
                  value={hex(header.flags)}
                  onChange={(e) => {
                    const parsed = Number.parseInt(e.target.value.replace(/^0x/i, ""), 16);
                    if (!Number.isNaN(parsed)) setHeader({ ...header, flags: parsed >>> 0 });
                  }}
                />
                <div className={styles.bitGrid}>
                  {FLAG_BITS.map((f) => (
                    <label key={f.bit} className={styles.bitToggle}>
                      <input
                        type="checkbox"
                        checked={((header.flags >>> f.bit) & 1) === 1}
                        onChange={() => toggleFlagBit(f.bit)}
                      />
                      {f.label}
                    </label>
                  ))}
                </div>
                <p className={styles.hint}>
                  Bit meanings marked (?) are inferred from shipped assets, not verified in-engine.
                </p>
              </div>

              <div className={styles.field}>
                <label className={styles.fieldLabel}>A/V material</label>
                <input
                  className={styles.hexInput}
                  value={hex(header.av_material_hash)}
                  onChange={(e) => {
                    const parsed = Number.parseInt(e.target.value.replace(/^0x/i, ""), 16);
                    if (!Number.isNaN(parsed))
                      setHeader({ ...header, av_material_hash: parsed >>> 0 });
                  }}
                />
                <span className={styles.dimmed}>{doc.av_material_name ?? "unknown name"}</span>
              </div>

              <div className={styles.field}>
                <label className={styles.fieldLabel}>Audio material</label>
                <input
                  className={styles.hexInput}
                  value={hex(header.audio_material_hash)}
                  onChange={(e) => {
                    const parsed = Number.parseInt(e.target.value.replace(/^0x/i, ""), 16);
                    if (!Number.isNaN(parsed))
                      setHeader({ ...header, audio_material_hash: parsed >>> 0 });
                  }}
                />
                <span className={styles.dimmed}>
                  {doc.audio_material_name ?? "0 = inherit A/V material"}
                </span>
              </div>

              <div className={styles.field}>
                <label className={styles.fieldLabel}>Unknown float @0x14</label>
                <input
                  type="number"
                  step="0.01"
                  className={styles.numInput}
                  value={header.unk14}
                  onChange={(e) => setHeader({ ...header, unk14: Number(e.target.value) })}
                />
              </div>

              <div className={styles.field}>
                <label className={styles.fieldLabel}>Unknown float @0x1C</label>
                <input
                  type="number"
                  step="0.01"
                  className={styles.numInput}
                  value={header.unk1c}
                  onChange={(e) => setHeader({ ...header, unk1c: Number(e.target.value) })}
                />
              </div>

              <p className={styles.hint}>Sections: {doc.section_tags.join(", ")}</p>
            </div>
          )}

          <div className={styles.actions}>
            <FilePickerInput
              label="Output .material (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={MATERIAL_FILTER}
              placeholder="Leave blank — saves as _edited"
            />
            <div style={{ display: "flex", justifyContent: "flex-end", gap: "0.6rem" }}>
              <button
                className={styles.runBtn}
                style={{
                  background: "transparent",
                  border: "1px solid var(--border)",
                  color: "var(--text-secondary)",
                }}
                onClick={() => setSendToStager(outPath || materialPath)}
                disabled={running}
                title="Send output to a Stager project"
              >
                Send to Stager
              </button>
              <button className={styles.runBtn} onClick={save} disabled={running || !dirty}>
                {running ? "Saving…" : "Save Material"}
              </button>
            </div>
          </div>
        </>
      )}

      {zoom && (
        <div className={styles.zoomCard} style={{ left: zoom.x, top: zoom.y }}>
          <img className={styles.zoomImage} src={zoom.src} alt="" />
          <span className={styles.zoomLabel}>{zoom.label}</span>
        </div>
      )}

      <StatusLog entries={log} />

      {sendToStager && (
        <SendToStagerModal
          sourceFile={sendToStager}
          defaultTargetPath={deriveStagerTarget(sendToStager, assetPath)}
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
