import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./ModelConverter.module.css";

import { FaArrowRight } from "react-icons/fa";

const MODEL_FILTER = [{ name: "Model Files", extensions: ["model"] }];
const ASCII_FILTER = [{ name: "ASCII Files", extensions: ["ascii"] }];
const GLTF_FILTER = [{ name: "GLTF/GLB Files", extensions: ["gltf", "glb"] }];

type Tab = "to-ascii" | "to-model";

interface LookGroupInfo {
  index: number;
  name: string;
}

interface SlotRequest {
  name: string;
  path: string | null;
  required: boolean;
  meshes: string[];
}

interface SlotPlan {
  new_slots: SlotRequest[];
  existing: { name: string; path: string }[];
  hair: { groups: string[]; strands: number } | null;
  strip_hair: boolean | null;
}

function hairGroupName(name: string) {
  return name.replace(/^HairDescription_/, "");
}

function baseName(path: string) {
  return path.split(/[\\/]/).pop() ?? path;
}

export default function ModelConverter() {
  const location = useLocation();
  const [tab, setTab] = useState<Tab>("to-ascii");
  const [format, setFormat] = useState<"ascii" | "gltf">("ascii");

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

  // New material slots the glTF would add, waiting for their .material paths.
  const [slotPlan, setSlotPlan] = useState<SlotPlan | null>(null);
  const [slotPaths, setSlotPaths] = useState<Record<string, string>>({});
  const [rememberPaths, setRememberPaths] = useState(true);
  const [slotError, setSlotError] = useState("");
  const [stripHair, setStripHair] = useState(false);

  useEffect(() => {
    setSlotPlan(null);
    setSlotError("");
  }, [asciiPath, srcModelPath, format, tab]);

  useEffect(() => {
    if (location.pathname !== "/tools/model-converter") return;
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
  }, [location.pathname, location.state, location.search]);

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
        const cmd = format === "gltf" ? "model_to_gltf" : "model_to_ascii";
        const result: string = await invoke(cmd, {
          modelPath,
          [format === "gltf" ? "gltfPath" : "asciiPath"]: asciiOutPath || null,
          look: lookIdx,
        });
        pushLog("success", `Done → ${result}`);
      } else {
        pushLog("info", `Converting ${modelPath} for ${looks.length} look group(s) into a single ${format.toUpperCase()} …`);
        const cmd = format === "gltf" ? "model_to_gltf" : "model_to_ascii";
        const result: string = await invoke(cmd, {
          modelPath,
          [format === "gltf" ? "gltfPath" : "asciiPath"]: asciiOutPath || null,
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

  // `confirmed` is set once the user has answered the import prompt.
  async function runAsciiToModel(confirmed?: { materials: Record<string, string>; stripHair: boolean | null }) {
    if (!asciiPath) { pushLog("error", `Select a .${format} file first.`); return; }
    if (!srcModelPath) { pushLog("error", "Select a source .model file first."); return; }
    const outputPath = overwriteSourceModel ? srcModelPath : (modelOutPath || null);
    setRunning(true);
    setLog([]);
    try {
      if (format === "gltf" && !confirmed) {
        const plan: SlotPlan = await invoke("gltf_material_slots", { gltfPath: asciiPath, srcModelPath });
        const missing = plan.new_slots.filter((s) => !s.path).map((s) => s.name);
        if (missing.length > 0 || plan.hair) {
          setSlotPlan(plan);
          setSlotPaths(Object.fromEntries(plan.new_slots.map((s) => [s.name, s.path ?? ""])));
          setSlotError("");
          setStripHair(plan.strip_hair ?? false);
          if (missing.length > 0) pushLog("warning", `New material slot(s) need a .material path: ${missing.join(", ")}`);
          if (plan.hair) pushLog("info", `Target model has ${plan.hair.groups.length} hair group(s); choose whether to keep them.`);
          return;
        }
      }
      pushLog("info", `Injecting ${asciiPath} → ${srcModelPath} …`);
      const result: string = format === "gltf"
        ? await invoke("gltf_to_model", {
            gltfPath: asciiPath,
            srcModelPath,
            outPath: outputPath,
            materials: confirmed?.materials ?? null,
            stripHair: confirmed?.stripHair ?? null,
            remember: rememberPaths,
          })
        : await invoke("ascii_to_model", { asciiPath, srcModelPath, outPath: outputPath });
      setSlotPlan(null);
      pushLog("success", `Done → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function confirmSlotPaths() {
    if (!slotPlan) return;
    const materials: Record<string, string> = {};
    for (const s of slotPlan.new_slots) {
      const path = (slotPaths[s.name] ?? "").trim().replace(/\//g, "\\");
      if (!path) {
        if (s.required) { setSlotError(`"${s.name}" needs a .material path.`); return; }
        continue;
      }
      if (!path.toLowerCase().endsWith(".material")) {
        setSlotError(`"${s.name}": the path should end in .material.`);
        return;
      }
      materials[s.name] = path;
    }
    setSlotError("");
    runAsciiToModel({ materials, stripHair: slotPlan.hair ? stripHair : null });
  }

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>Model Converter</h2>
        <span className={styles.subtitle}>Export .model mesh data to .ascii or .gltf for editing, and import it back</span>
      </div>

      <div className={styles.tabs}>
        <button className={`${styles.tab} ${tab === "to-ascii" ? styles.active : ""}`} onClick={() => setTab("to-ascii")}>
          Model <FaArrowRight /> Editor
        </button>
        <button className={`${styles.tab} ${tab === "to-model" ? styles.active : ""}`} onClick={() => setTab("to-model")}>
          Editor <FaArrowRight /> Model
        </button>
      </div>

      {tab === "to-ascii" && (
        <div className={styles.panel}>
          <div style={{ display: 'flex', gap: '1rem', marginBottom: '1rem' }}>
            <label style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
              <input type="radio" checked={format === "ascii"} onChange={() => setFormat("ascii")} /> ASCII Mode
            </label>
            <label style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
              <input type="radio" checked={format === "gltf"} onChange={() => setFormat("gltf")} /> GLTF Mode
            </label>
          </div>
          <FilePickerInput label="Source .model" value={modelPath} onChange={setModelPath} mode="open" filters={MODEL_FILTER} />
          <FilePickerInput label={`Output .${format === "gltf" ? "glb" : format} (optional)`} value={asciiOutPath} onChange={setAsciiOutPath} mode="save" filters={format === "gltf" ? GLTF_FILTER : ASCII_FILTER} placeholder="Leave blank for auto" />
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
                ? `Export ${selectedLooks.size} Look Groups to ${format.toUpperCase()}`
                : `Export to ${format.toUpperCase()}`}
          </button>
        </div>
      )}

      {tab === "to-model" && (
        <div className={styles.panel}>
          <div style={{ display: 'flex', gap: '1rem', marginBottom: '1rem' }}>
            <label style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
              <input type="radio" checked={format === "ascii"} onChange={() => setFormat("ascii")} /> ASCII Mode
            </label>
            <label style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
              <input type="radio" checked={format === "gltf"} onChange={() => setFormat("gltf")} /> GLTF Mode
            </label>
          </div>
          <FilePickerInput
            label={`Source .${format}`}
            value={asciiPath}
            onChange={setAsciiPath}
            mode="open"
            filters={format === "gltf" ? GLTF_FILTER : ASCII_FILTER}
          />
          <FilePickerInput label="Target .model" value={srcModelPath} onChange={setSrcModelPath} mode="open" filters={MODEL_FILTER} />
          <FilePickerInput
            label="Output .model (optional — defaults to source with _modified suffix)"
            value={modelOutPath}
            onChange={setModelOutPath}
            mode="save"
            filters={MODEL_FILTER}
            placeholder="Leave blank for auto"
          />
          {format === "gltf" && slotPlan && (
            <div className={styles.slotPrompt}>
              {slotPlan.hair && (
                <>
                  <div className={styles.slotPromptTitle}>Hair / fur strands</div>
                  <p className={styles.slotPromptHint}>
                    {slotPlan.hair.groups.map(hairGroupName).join(", ")} ({slotPlan.hair.strands.toLocaleString()} strands).
                    Bound to the original meshes; remove if you replaced them.
                  </p>
                  <label className={styles.overwriteToggle}>
                    <input type="radio" checked={!stripHair} onChange={() => setStripHair(false)} />
                    <span>Keep</span>
                  </label>
                  <label className={styles.overwriteToggle}>
                    <input type="radio" checked={stripHair} onChange={() => setStripHair(true)} />
                    <span>Remove all</span>
                  </label>
                </>
              )}
              {slotPlan.new_slots.length > 0 && (
                <>
                  <div className={styles.slotPromptTitle}>New material slots</div>
                  <p className={styles.slotPromptHint}>
                    Not in the target model. Enter a .material path, or rename to an existing slot.
                  </p>
                </>
              )}
              {slotPlan.new_slots.map((s) => (
                <label key={s.name} className={styles.slotRow}>
                  <span className={styles.slotName}>{s.name}</span>
                  <span className={styles.slotMeshes}>used by {s.meshes.join(", ")}</span>
                  <input
                    className={styles.slotInput}
                    list="omni-model-materials"
                    value={slotPaths[s.name] ?? ""}
                    onChange={(e) => setSlotPaths((prev) => ({ ...prev, [s.name]: e.target.value.replace(/\//g, "\\") }))}
                    placeholder={s.required
                      ? "material\\characters\\…\\name.material"
                      : "optional: blank keeps the current slot"}
                    spellCheck={false}
                  />
                </label>
              ))}
              <datalist id="omni-model-materials">
                {[...new Map(slotPlan.existing.filter((e) => e.path).map((e) => [e.path, e.name])).entries()].map(([path, name]) => (
                  <option key={path} value={path}>{name}</option>
                ))}
              </datalist>
              <label className={styles.overwriteToggle}>
                <input type="checkbox" checked={rememberPaths} onChange={(e) => setRememberPaths(e.target.checked)} />
                <span>Remember for this glTF ({baseName(asciiPath)}.omni.json)</span>
              </label>
              {slotError && <div className={styles.slotError}>{slotError}</div>}
              <div className={styles.injectActionsRow}>
                <button className={styles.runBtn} onClick={confirmSlotPaths} disabled={running}>
                  {running ? "Injecting…" : "Continue import"}
                </button>
                <button type="button" className={styles.linkBtn} onClick={() => setSlotPlan(null)} disabled={running}>
                  Cancel
                </button>
              </div>
            </div>
          )}
          <div className={styles.injectActionsRow} hidden={format === "gltf" && !!slotPlan}>
            <button className={styles.runBtn} onClick={() => runAsciiToModel()} disabled={running}>
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
