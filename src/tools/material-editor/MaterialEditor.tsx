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
  template_ptr: number;
  flags: number;
  av_material_hash: number;
  audio_material_hash: number;
  alpha: number;
  alpha_test: number;
  lod_dist: number;
  voxelization_order_bias: number;
  pad: number[];
}

interface SamplerView {
  name_hash: number;
  name: string;
  path: string;
  slot_index: number | null;
  default_path: string | null;
  in_template: boolean;
  overridden: boolean;
  user_exposed: boolean;
  sampler_type: string | null;
}

interface ConstantView {
  name_hash: number;
  name: string;
  values: number[];
  default_values: number[] | null;
  in_template: boolean;
  overridden: boolean;
}

interface NamedHash {
  name_hash: number;
  name: string;
}

interface VariationView extends NamedHash {
  value: number;
}

interface FurInfo {
  layer_count: number;
  lod_reduction: number;
  length: number;
  density: number;
  offset_scale: number;
  gloss_scale: number;
  specular_scale: number;
  transmittance_scale: number;
  wetness: number;
  map_offsets: number[];
}

interface FurDoc {
  info: FurInfo;
  maps: (string | null)[];
}

interface WaterInfo {
  water_color: number[];
  water_scale: number;
  foam_color: number[];
  foam_amp: number;
  foam_power: number;
  water_depth: number;
  darkening: number;
  flow_rate: number;
  flow_phase: number;
  flow_noise: number;
  caustics_intensity: number;
  caustics_depth_bias: number;
  water_gloss: number;
  water_color_scale: number;
  flow_map_offset: number;
}

interface WaterDoc {
  info: WaterInfo;
  flow_map: string | null;
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
  embedded_template: boolean;
  variations: VariationView[];
  fur: FurDoc | null;
  water: WaterDoc | null;
  av_materials: NamedHash[];
  section_tags: string[];
}

type Tab = "textures" | "constants" | "header" | "fur" | "water";

const BIT = {
  DoubleSided: 0,
  AccurateAlphaVelocity: 1,
  SkipShadowCast: 2,
  ShadowCastOnly: 3,
  CastOpaqueShadow: 4,
  SkipLightCapture: 5,
  SkipEmbeddedTest: 6,
  AlphaLit: 11,
  SSRDisabled: 12,
  UseAoOnDecals: 13,
  Fur: 14,
  AllowTriangleSorting: 15,
  OverlapColorOnly: 16,
  ForceOpaqueLoDs: 17,
  AlphaDepthPass: 18,
  ImpostorHQModel: 19,
  OverlapNormalOnly: 20,
  DisableLensFlareOcclusion: 21,
  AlphaTemporalAAResponsive: 22,
  Water: 23,
  SSRFidelityOnly: 24,
} as const;

// Bits 7-10 hold one blend mode; shipped materials never set more than one.
const BLEND_MODES: { label: string; bit: number | null }[] = [
  { label: "Opaque", bit: null },
  { label: "Alpha", bit: 8 },
  { label: "Additive", bit: 7 },
  { label: "Modulate", bit: 9 },
  { label: "Hybrid", bit: 10 },
];

const SSR_MODES: { label: string; bit: number | null }[] = [
  { label: "Allowed", bit: null },
  { label: "Disabled", bit: BIT.SSRDisabled },
  { label: "30 fps mode only", bit: BIT.SSRFidelityOnly },
];

const OVERLAP_MODES: { label: string; bit: number | null }[] = [
  { label: "Full", bit: null },
  { label: "Color only", bit: BIT.OverlapColorOnly },
  { label: "Normal only", bit: BIT.OverlapNormalOnly },
];

const FLAG_GROUPS: { title: string; bits: { bit: number; label: string; hint?: string }[] }[] = [
  {
    title: "Transparency",
    bits: [
      { bit: BIT.AlphaLit, label: "Lit when blended", hint: "Blended surfaces receive lighting. On by default, so most opaque materials have it too." },
      { bit: BIT.AlphaDepthPass, label: "Alpha depth pass" },
      { bit: BIT.ForceOpaqueLoDs, label: "Opaque distant LODs" },
      { bit: BIT.AlphaTemporalAAResponsive, label: "Responsive TAA" },
    ],
  },
  {
    title: "Geometry",
    bits: [
      { bit: BIT.DoubleSided, label: "Double sided" },
      { bit: BIT.AllowTriangleSorting, label: "Sort triangles", hint: "Used on transparent parts like lenses and lashes." },
      { bit: BIT.UseAoOnDecals, label: "AO on decals" },
      { bit: BIT.ImpostorHQModel, label: "Skip impostor projection" },
    ],
  },
  {
    title: "Shadows",
    bits: [
      { bit: BIT.CastOpaqueShadow, label: "Cast opaque shadow" },
      { bit: BIT.SkipShadowCast, label: "No shadow" },
      { bit: BIT.ShadowCastOnly, label: "Shadow only (invisible)" },
    ],
  },
  {
    title: "Lighting",
    bits: [
      { bit: BIT.SkipLightCapture, label: "Skip light capture" },
      { bit: BIT.DisableLensFlareOcclusion, label: "No lens flare occlusion" },
      { bit: BIT.SkipEmbeddedTest, label: "Skip embedded test" },
    ],
  },
];

const FUR_FIELDS: { key: keyof FurInfo; label: string; step: number }[] = [
  { key: "layer_count", label: "Layer count", step: 1 },
  { key: "length", label: "Length", step: 0.005 },
  { key: "density", label: "Density", step: 0.5 },
  { key: "lod_reduction", label: "LoD reduction", step: 0.05 },
  { key: "offset_scale", label: "Offset scale", step: 0.05 },
  { key: "gloss_scale", label: "Gloss scale", step: 0.05 },
  { key: "specular_scale", label: "Specular scale", step: 0.05 },
  { key: "transmittance_scale", label: "Transmittance scale", step: 0.05 },
  { key: "wetness", label: "Wetness", step: 0.05 },
];
const FUR_MAPS = ["Base map", "Normal map", "Gloss map", "Control map"];

type WaterScalar = Exclude<keyof WaterInfo, "water_color" | "foam_color" | "flow_map_offset">;
const WATER_FIELDS: { key: WaterScalar; label: string }[] = [
  { key: "water_scale", label: "Water scale" },
  { key: "water_color_scale", label: "Color scale" },
  { key: "water_depth", label: "Depth" },
  { key: "darkening", label: "Darkening" },
  { key: "water_gloss", label: "Gloss" },
  { key: "foam_amp", label: "Foam amount" },
  { key: "foam_power", label: "Foam power" },
  { key: "flow_rate", label: "Flow rate" },
  { key: "flow_phase", label: "Flow phase" },
  { key: "flow_noise", label: "Flow noise" },
  { key: "caustics_intensity", label: "Caustics intensity" },
  { key: "caustics_depth_bias", label: "Caustics depth bias" },
];

function hex(n: number): string {
  return `0x${(n >>> 0).toString(16).toUpperCase().padStart(8, "0")}`;
}

function hasBit(flags: number, bit: number): boolean {
  return ((flags >>> bit) & 1) === 1;
}

function withBit(flags: number, bit: number, on: boolean): number {
  return (on ? flags | (1 << bit) : flags & ~(1 << bit)) >>> 0;
}

// Clears every bit of an exclusive group, then sets `bit` (if any).
function withChoice(flags: number, group: (number | null)[], bit: number | null): number {
  let f = flags;
  for (const b of group) if (b !== null) f = withBit(f, b, false);
  return bit === null ? f : withBit(f, bit, true);
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

function samplerType(t: string | null): string {
  return t ? t.replace(/^sampler/, "") : "—";
}

function slotState(s: SamplerView): string {
  if (!s.user_exposed) return "locked by template";
  if (s.overridden && !s.in_template) return "unused (not in template)";
  return s.overridden ? "override" : "template default";
}

function AvSelect({
  value,
  options,
  allowUnset,
  onChange,
}: {
  value: number;
  options: NamedHash[];
  allowUnset: boolean;
  onChange: (v: number) => void;
}) {
  const known = value === 0 || options.some((o) => o.name_hash === value);
  return (
    <select
      className={styles.select}
      value={value}
      onChange={(e) => onChange(Number(e.target.value) >>> 0)}
    >
      {allowUnset && <option value={0}>unset (kNone)</option>}
      {!known && <option value={value}>{hex(value)} (unknown name)</option>}
      {options.map((o) => (
        <option key={o.name_hash} value={o.name_hash}>
          {o.name.replace(/^k/, "")}
        </option>
      ))}
    </select>
  );
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
  const [fur, setFur] = useState<FurDoc | null>(null);
  const [water, setWater] = useState<WaterDoc | null>(null);
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
      setFur(result.fur);
      setWater(result.water);
      setTab(result.fur ? "fur" : result.water ? "water" : "textures");
      pushLog(
        "success",
        `Loaded ${result.samplers.length} texture slot(s), ${result.constants.length} constant(s).`,
      );
      if (result.embedded_template) {
        pushLog("info", "Slots come from the material's own compiled template (shader variations).");
      } else if (result.template_path) {
        if (result.template_loaded) {
          pushLog("info", `Template: ${result.template_path}`);
        } else {
          pushLog(
            "warning",
            `Template ${result.template_path} not loaded — ${result.template_note}`,
          );
        }
      }
      if (result.fur) pushLog("info", "Built-in fur material: settings are on the Fur tab.");
      if (result.water) pushLog("info", "Built-in water material: settings are on the Water tab.");
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
        fur,
        water,
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

  function setFlags(flags: number) {
    setHeader((prev) => (prev ? { ...prev, flags: flags >>> 0 } : prev));
  }

  function patchWater(patch: Partial<WaterInfo>) {
    setWater((prev) => (prev ? { ...prev, info: { ...prev.info, ...patch } } : prev));
  }

  const dirty =
    doc !== null &&
    (JSON.stringify(samplers) !== JSON.stringify(doc.samplers) ||
      JSON.stringify(constants) !== JSON.stringify(doc.constants) ||
      JSON.stringify(header) !== JSON.stringify(doc.header) ||
      JSON.stringify(fur) !== JSON.stringify(doc.fur) ||
      JSON.stringify(water) !== JSON.stringify(doc.water));

  const tabs: Tab[] = ["textures", "constants", "header"];
  if (fur) tabs.unshift("fur");
  if (water) tabs.unshift("water");
  const tabLabel = (t: Tab) =>
    t === "textures"
      ? `Textures (${samplers.length})`
      : t === "constants"
        ? `Constants (${constants.length})`
        : t === "header"
          ? "Render & physics"
          : t === "fur"
            ? "Fur"
            : "Water";

  const choice = (modes: { bit: number | null }[]) =>
    header ? (modes.find((m) => m.bit !== null && hasBit(header.flags, m.bit))?.bit ?? null) : null;

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>Material Editor</h2>
        <span className={styles.subtitle}>
          Edit texture slots, shader constants, render flags and fur/water settings inside .material files
        </span>
      </div>

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
            {doc.embedded_template ? (
              <span
                className={styles.okBadge}
                title="This material carries its own compiled copy of the template (shader variations), so slots are read from the material itself."
              >
                embedded copy
              </span>
            ) : (
              doc.template_path && (
                <span className={doc.template_loaded ? styles.okBadge : styles.warnBadge}>
                  {doc.template_loaded ? "slots resolved" : "slots unresolved"}
                </span>
              )
            )}
            {fur && <span className={styles.badge}>fur</span>}
            {water && <span className={styles.badge}>water</span>}
          </div>

          <div className={styles.tabs}>
            {tabs.map((t) => (
              <button
                key={t}
                className={`${styles.tab} ${tab === t ? styles.active : ""}`}
                onClick={() => setTab(t)}
              >
                {tabLabel(t)}
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
                    <th className={styles.th}>Type</th>
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
                      <td className={`${styles.td} ${styles.dimmed}`}>{samplerType(s.sampler_type)}</td>
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
                            disabled={!s.user_exposed}
                            title={
                              s.user_exposed
                                ? undefined
                                : "The template doesn't expose this slot, so the game ignores material overrides of it."
                            }
                            onChange={(e) => updateSampler(s.name_hash, e.target.value)}
                          />
                        </div>
                      </td>
                      <td className={`${styles.td} ${styles.dimmed}`}>{slotState(s)}</td>
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
                          disabled={s.default_path === null || !s.overridden || !s.user_exposed}
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
                        {c.overridden && !c.in_template && doc.template_loaded
                          ? "unused (not in template)"
                          : c.overridden
                            ? "override"
                            : "template default"}
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
                  {doc.variations.length > 0 && (
                    <>
                      <tr>
                        <td className={`${styles.td} ${styles.sectionRow}`} colSpan={5}>
                          Shader variations — compiled into the material's shaders, shown read-only
                        </td>
                      </tr>
                      {doc.variations.map((v) => (
                        <tr key={`var-${v.name_hash}`}>
                          <td className={styles.td} title={hex(v.name_hash)}>
                            {v.name}
                          </td>
                          <td className={styles.td}>{v.value ? "on" : "off"}</td>
                          <td className={styles.td} colSpan={3} />
                        </tr>
                      ))}
                    </>
                  )}
                </tbody>
              </table>
            </div>
          )}

          {tab === "header" && header && (
            <div className={styles.headerPanel}>
              <div className={styles.groupGrid}>
                <div className={styles.group}>
                  <span className={styles.groupTitle}>Blending</span>
                  <div className={styles.radioRow}>
                    {BLEND_MODES.map((m) => (
                      <label key={m.label} className={styles.bitToggle}>
                        <input
                          type="radio"
                          name="blend"
                          checked={choice(BLEND_MODES) === m.bit}
                          onChange={() => setFlags(withChoice(header.flags, BLEND_MODES.map((x) => x.bit), m.bit))}
                        />
                        {m.label}
                      </label>
                    ))}
                  </div>
                  <div className={styles.formRow}>
                    <label className={styles.formLabel} title="Material opacity; 1 is fully opaque.">
                      Alpha
                    </label>
                    <input
                      type="number"
                      step="0.05"
                      className={styles.numInput}
                      value={header.alpha}
                      onChange={(e) => setHeader({ ...header, alpha: Number(e.target.value) })}
                    />
                  </div>
                  <div className={styles.formRow}>
                    <label
                      className={styles.formLabel}
                      title="Alpha-test cutoff. Blended materials with 0 get a small default cutoff in game."
                    >
                      Alpha test
                    </label>
                    <input
                      type="number"
                      step="0.05"
                      className={styles.numInput}
                      value={header.alpha_test}
                      onChange={(e) => setHeader({ ...header, alpha_test: Number(e.target.value) })}
                    />
                  </div>
                </div>

                {FLAG_GROUPS.map((g) => (
                  <div key={g.title} className={styles.group}>
                    <span className={styles.groupTitle}>{g.title}</span>
                    {g.bits.map((f) => (
                      <label key={f.bit} className={styles.bitToggle} title={f.hint}>
                        <input
                          type="checkbox"
                          checked={hasBit(header.flags, f.bit)}
                          onChange={(e) => setFlags(withBit(header.flags, f.bit, e.target.checked))}
                        />
                        {f.label}
                      </label>
                    ))}
                    {g.title === "Geometry" && (
                      <div className={styles.radioRow}>
                        <span className={styles.dimmed}>Overlap</span>
                        {OVERLAP_MODES.map((m) => (
                          <label key={m.label} className={styles.bitToggle}>
                            <input
                              type="radio"
                              name="overlap"
                              checked={choice(OVERLAP_MODES) === m.bit}
                              onChange={() => setFlags(withChoice(header.flags, OVERLAP_MODES.map((x) => x.bit), m.bit))}
                            />
                            {m.label}
                          </label>
                        ))}
                      </div>
                    )}
                    {g.title === "Lighting" && (
                      <div className={styles.radioRow}>
                        <span className={styles.dimmed}>Screen-space reflections</span>
                        {SSR_MODES.map((m) => (
                          <label key={m.label} className={styles.bitToggle}>
                            <input
                              type="radio"
                              name="ssr"
                              checked={choice(SSR_MODES) === m.bit}
                              onChange={() => setFlags(withChoice(header.flags, SSR_MODES.map((x) => x.bit), m.bit))}
                            />
                            {m.label}
                          </label>
                        ))}
                      </div>
                    )}
                  </div>
                ))}

                <div className={styles.group}>
                  <span className={styles.groupTitle}>Compiled / built-in</span>
                  <label className={styles.bitToggle} title="Compiled into the material's shaders; can't be changed here.">
                    <input type="checkbox" checked={hasBit(header.flags, BIT.AccurateAlphaVelocity)} disabled />
                    Accurate alpha velocity
                  </label>
                  <label className={styles.bitToggle} title="Needs the Fur Info section; can't be changed here.">
                    <input type="checkbox" checked={hasBit(header.flags, BIT.Fur)} disabled />
                    Fur
                  </label>
                  <label className={styles.bitToggle} title="Needs the Water Info section; can't be changed here.">
                    <input type="checkbox" checked={hasBit(header.flags, BIT.Water)} disabled />
                    Water
                  </label>
                </div>

                <div className={styles.group}>
                  <span className={styles.groupTitle}>Physics & audio</span>
                  <div className={styles.formRow}>
                    <label className={styles.formLabel} title="Impact effects and physics surface type.">
                      A/V material
                    </label>
                    <AvSelect
                      value={header.av_material_hash}
                      options={doc.av_materials}
                      allowUnset
                      onChange={(v) => setHeader({ ...header, av_material_hash: v })}
                    />
                  </div>
                  <div className={styles.formRow}>
                    <label
                      className={styles.formLabel}
                      title="Footstep and impact sounds. Shipped materials store the A/V material here unless they set their own."
                    >
                      Audio material
                    </label>
                    <AvSelect
                      value={header.audio_material_hash}
                      options={doc.av_materials}
                      allowUnset={false}
                      onChange={(v) => setHeader({ ...header, audio_material_hash: v })}
                    />
                  </div>
                </div>

                <div className={styles.group}>
                  <span className={styles.groupTitle}>Distance</span>
                  <div className={styles.formRow}>
                    <label className={styles.formLabel} title="Material LOD switch distance; 0 uses the default.">
                      LOD distance
                    </label>
                    <input
                      type="number"
                      step="1"
                      className={styles.numInput}
                      value={header.lod_dist}
                      onChange={(e) => setHeader({ ...header, lod_dist: Number(e.target.value) })}
                    />
                  </div>
                  <div className={styles.formRow}>
                    <label className={styles.formLabel} title="0 is neutral; stored with a +128 bias.">
                      Voxelization order bias
                    </label>
                    <input
                      type="number"
                      step="1"
                      min={-128}
                      max={127}
                      className={styles.numInput}
                      value={header.voxelization_order_bias - 128}
                      onChange={(e) => {
                        const v = Math.max(-128, Math.min(127, Math.round(Number(e.target.value))));
                        setHeader({ ...header, voxelization_order_bias: v + 128 });
                      }}
                    />
                  </div>
                </div>
              </div>

              <div className={styles.field}>
                <label className={styles.fieldLabel}>Raw flags</label>
                <input
                  className={styles.hexInput}
                  value={hex(header.flags)}
                  onChange={(e) => {
                    const parsed = Number.parseInt(e.target.value.replace(/^0x/i, ""), 16);
                    if (!Number.isNaN(parsed)) setFlags(parsed);
                  }}
                />
                {header.flags >>> 25 !== 0 && (
                  <p className={styles.hint}>Bits 25–31 are runtime-only; saving is blocked while they are set.</p>
                )}
              </div>

              <p className={styles.hint}>Sections: {doc.section_tags.join(", ")}</p>
            </div>
          )}

          {tab === "fur" && fur && (
            <div className={styles.headerPanel}>
              <div className={styles.group}>
                <span className={styles.groupTitle}>Fur shells</span>
                <div className={styles.formGrid}>
                  {FUR_FIELDS.map((f) => (
                    <div key={f.key} className={styles.formRow}>
                      <label className={styles.formLabel}>{f.label}</label>
                      <input
                        type="number"
                        step={f.step}
                        min={f.key === "layer_count" ? 0 : undefined}
                        className={styles.numInput}
                        value={fur.info[f.key] as number}
                        onChange={(e) => {
                          const n = Number(e.target.value);
                          const v = f.key === "layer_count" ? Math.max(0, Math.round(n)) : n;
                          setFur({ ...fur, info: { ...fur.info, [f.key]: v } });
                        }}
                      />
                    </div>
                  ))}
                </div>
              </div>
              <div className={styles.group}>
                <span className={styles.groupTitle}>Fur maps</span>
                {FUR_MAPS.map((label, i) => (
                  <div key={label} className={styles.formRow}>
                    <label className={styles.formLabel}>{label}</label>
                    <input
                      className={styles.pathInput}
                      value={fur.maps[i] ?? ""}
                      placeholder="none"
                      onChange={(e) => {
                        const maps = fur.maps.slice();
                        maps[i] = e.target.value || null;
                        setFur({ ...fur, maps });
                      }}
                    />
                  </div>
                ))}
                <p className={styles.hint}>
                  New map paths are appended to the material's string pool. The texture must also be
                  loaded by the zone, or the game falls back to its default.
                </p>
              </div>
            </div>
          )}

          {tab === "water" && water && (
            <div className={styles.headerPanel}>
              <div className={styles.group}>
                <span className={styles.groupTitle}>Colors</span>
                {(["water_color", "foam_color"] as const).map((key) => (
                  <div key={key} className={styles.formRow}>
                    <label className={styles.formLabel}>{key === "water_color" ? "Water color" : "Foam color"}</label>
                    <div className={styles.valueCell}>
                      <input
                        type="color"
                        className={styles.colorInput}
                        value={toHexColor(water.info[key])}
                        onChange={(e) => patchWater({ [key]: fromHexColor(e.target.value, 3, water.info[key]) })}
                      />
                      {water.info[key].map((v, i) => (
                        <input
                          key={i}
                          type="number"
                          step="0.01"
                          className={styles.numInput}
                          value={v}
                          onChange={(e) => {
                            const c = water.info[key].slice();
                            c[i] = Number(e.target.value);
                            patchWater({ [key]: c });
                          }}
                        />
                      ))}
                    </div>
                  </div>
                ))}
              </div>
              <div className={styles.group}>
                <span className={styles.groupTitle}>Surface</span>
                <div className={styles.formGrid}>
                  {WATER_FIELDS.map((f) => (
                    <div key={f.key} className={styles.formRow}>
                      <label className={styles.formLabel}>{f.label}</label>
                      <input
                        type="number"
                        step="0.01"
                        className={styles.numInput}
                        value={water.info[f.key]}
                        onChange={(e) => patchWater({ [f.key]: Number(e.target.value) })}
                      />
                    </div>
                  ))}
                </div>
                <div className={styles.formRow}>
                  <label className={styles.formLabel}>Flow map</label>
                  <input
                    className={styles.pathInput}
                    value={water.flow_map ?? ""}
                    placeholder="none"
                    onChange={(e) => setWater({ ...water, flow_map: e.target.value || null })}
                  />
                </div>
              </div>
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
