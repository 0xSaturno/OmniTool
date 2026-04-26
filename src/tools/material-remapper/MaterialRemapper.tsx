import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./MaterialRemapper.module.css";

const MODEL_FILTER = [{ name: "Insomniac Model", extensions: ["model"] }];

interface MaterialSlotInfo {
  index: number;
  path: string;
  name: string;
}

interface SubmeshInfo {
  index: number;
  material_index: number;
  vertex_count: number;
  face_count: number;
  look_indices: number[];
}

interface ModelMaterialData {
  materials: MaterialSlotInfo[];
  submeshes: SubmeshInfo[];
  look_names: string[];
}

type Tab = "slots" | "submeshes";

export default function MaterialRemapper() {
  const location = useLocation();
  const [modelPath, setModelPath] = useState("");
  const [outPath, setOutPath] = useState("");
  const [tab, setTab] = useState<Tab>("slots");
  const [sendToStager, setSendToStager] = useState<string | null>(null);
  const [assetPath, setAssetPath] = useState<string | null>(null);

  const [data, setData] = useState<ModelMaterialData | null>(null);
  const [edits, setEdits] = useState<Map<number, string>>(new Map());

  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    const s = location.state as { filePath?: string; assetPath?: string } | null;
    if (s?.filePath) setModelPath(s.filePath);
    if (s?.assetPath) setAssetPath(s.assetPath);
  }, [location.state]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadMaterials() {
    if (!modelPath) {
      pushLog("error", "Select a .model file first.");
      return;
    }
    setRunning(true);
    setLog([]);
    setData(null);
    setEdits(new Map());
    try {
      pushLog("info", `Reading materials from ${modelPath} …`);
      const result: ModelMaterialData = await invoke("read_model_materials", { modelPath });
      setData(result);
      pushLog("success", `Loaded ${result.materials.length} material slot(s) and ${result.submeshes.length} submesh(es).`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function saveChanges() {
    if (!modelPath || edits.size === 0) return;
    setRunning(true);
    try {
      pushLog("info", `Saving ${edits.size} change(s) …`);
      const result: string = await invoke("save_model_materials", {
        modelPath,
        materials: Array.from(edits.entries()).map(([index, path]) => ({ index, path })),
        outPath: outPath || null,
      });
      pushLog("success", `Done → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  function setEditPath(index: number, newPath: string) {
    setEdits((prev) => {
      const next = new Map(prev);
      const original = data?.materials.find((m) => m.index === index)?.path;
      if (newPath === original) {
        next.delete(index);
      } else {
        next.set(index, newPath);
      }
      return next;
    });
  }

  function resolvedPath(materialIndex: number): string {
    if (edits.has(materialIndex)) return edits.get(materialIndex)!;
    return data?.materials.find((m) => m.index === materialIndex)?.path ?? "";
  }

  function lookGroups(materialIndex: number): string {
    if (!data) return "";
    const used = new Set<number>();
    for (const s of data.submeshes) {
      if (s.material_index === materialIndex) {
        for (const li of s.look_indices) used.add(li);
      }
    }
    if (used.size === 0) return "—";
    return [...used]
      .sort((a, b) => a - b)
      .map((li) => data.look_names[li] ?? `look ${li}`)
      .join(", ");
  }

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Material Remapper</h2>
      <p className={styles.subtitle}>Remap material path references inside .model files</p>

      <div className={styles.panel}>
        <FilePickerInput label="Source .model" value={modelPath} onChange={setModelPath} mode="open" filters={MODEL_FILTER} />
        <button className={styles.runBtn} onClick={loadMaterials} disabled={running}>
          {running ? "Loading…" : "Load Materials"}
        </button>
      </div>

      {data && (
        <>
          <div className={styles.tabs}>
            <button className={`${styles.tab} ${tab === "slots" ? styles.active : ""}`} onClick={() => setTab("slots")}>
              Material Slots
            </button>
            <button className={`${styles.tab} ${tab === "submeshes" ? styles.active : ""}`} onClick={() => setTab("submeshes")}>
              Submesh Details
            </button>
          </div>

          {tab === "slots" && (
            <div className={styles.tableWrap}>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th className={styles.th}>#</th>
                    <th className={`${styles.th} ${styles.thPath}`}>Material Path</th>
                    <th className={styles.th}>Mesh Part Name</th>
                    <th className={styles.th}>Look Group</th>
                  </tr>
                </thead>
                <tbody>
                  {data.materials.map((mat) => (
                    <tr key={mat.index} className={edits.has(mat.index) ? styles.changed : ""}>
                      <td className={styles.td}>{mat.index}</td>
                      <td className={styles.td}>
                        <input
                          className={styles.pathInput}
                          value={edits.has(mat.index) ? edits.get(mat.index)! : mat.path}
                          onChange={(e) => setEditPath(mat.index, e.target.value)}
                        />
                      </td>
                      <td className={`${styles.td} ${styles.dimmed}`}>{mat.name}</td>
                      <td className={styles.td}>{lookGroups(mat.index)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {tab === "submeshes" && (
            <div className={styles.tableWrap}>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th className={styles.th}>#</th>
                    <th className={styles.th}>Material</th>
                    <th className={styles.th}>Vertices</th>
                    <th className={styles.th}>Faces</th>
                    <th className={styles.th}>Looks</th>
                  </tr>
                </thead>
                <tbody>
                  {data.submeshes.map((sm) => (
                    <tr key={sm.index}>
                      <td className={styles.td}>{sm.index}</td>
                      <td className={`${styles.td} ${styles.dimmed}`} style={{ fontFamily: "var(--font-mono)", fontSize: "0.8rem" }}>
                        {resolvedPath(sm.material_index)}
                      </td>
                      <td className={styles.td}>{sm.vertex_count.toLocaleString()}</td>
                      <td className={styles.td}>{sm.face_count.toLocaleString()}</td>
                      <td className={styles.td}>
                        {sm.look_indices.map((l) => (
                          <span key={l} className={styles.badge}>{l}</span>
                        ))}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          <div className={styles.actions}>
            <FilePickerInput
              label="Output .model (optional)"
              value={outPath}
              onChange={setOutPath}
              mode="save"
              filters={MODEL_FILTER}
              placeholder="Leave blank — saves as _matmod"
            />
            <div style={{ display: "flex", justifyContent: "flex-end", gap: "0.6rem" }}>
              <button
                className={styles.runBtn}
                style={{ background: "transparent", border: "1px solid var(--border)", color: "var(--text-secondary)" }}
                onClick={() => setSendToStager(outPath || modelPath)}
                disabled={running || edits.size === 0}
                title="Send output to a Stager project"
              >
                Send to Stager
              </button>
              <button className={styles.runBtn} onClick={saveChanges} disabled={running || edits.size === 0}>
                {running ? "Saving…" : `Save Changes (${edits.size})`}
              </button>
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
