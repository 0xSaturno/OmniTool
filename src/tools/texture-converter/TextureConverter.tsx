import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./TextureConverter.module.css";

const TEXTURE_FILTER = [{ name: "Texture Asset", extensions: ["texture"] }];
const DDS_FILTER = [{ name: "DDS Texture", extensions: ["dds"] }];

interface TextureInfo {
  width: number;
  height: number;
  mipmaps: number;
  hdmipmaps: number;
  images: number;
  bytes_per_pixel: number;
  size: number;
  hdsize: number;
  format: number;
}

const FORMAT_MAP: Record<number, string> = {
  2: "DXGI_FORMAT_R32G32B32A32_FLOAT",
  10: "DXGI_FORMAT_R16G16B16A16_FLOAT",
  28: "DXGI_FORMAT_R8G8B8A8_UNORM",
  29: "DXGI_FORMAT_R8G8B8A8_UNORM_SRGB",
  71: "DXGI_FORMAT_BC1_UNORM",
  72: "DXGI_FORMAT_BC1_UNORM_SRGB",
  74: "DXGI_FORMAT_BC2_UNORM",
  75: "DXGI_FORMAT_BC2_UNORM_SRGB",
  77: "DXGI_FORMAT_BC3_UNORM",
  78: "DXGI_FORMAT_BC3_UNORM_SRGB",
  80: "DXGI_FORMAT_BC4_UNORM",
  83: "DXGI_FORMAT_BC5_UNORM",
  87: "DXGI_FORMAT_B8G8R8A8_UNORM",
  91: "DXGI_FORMAT_B8G8R8A8_UNORM_SRGB",
  61: "DXGI_FORMAT_R8_UNORM",
  95: "DXGI_FORMAT_BC6H_UF16",
  96: "DXGI_FORMAT_BC6H_SF16",
  98: "DXGI_FORMAT_BC7_UNORM",
  99: "DXGI_FORMAT_BC7_UNORM_SRGB"
};

type Tab = "extract" | "replace" | "batch";

interface TextureJob {
  base_name: string;
  sd_path: string | null;
  hd_path: string | null;
  dds_path: string | null;
  selected?: boolean;
}

interface ProjectInfo {
  name: string;
  game: string;
  author: string;
  version: string;
}

class AsyncSemaphore {
  tasks: (() => void)[] = [];
  active = 0;
  max: number;
  constructor(max: number) { this.max = max; }
  async acquire() {
    if (this.active < this.max) {
      this.active++;
      return;
    }
    return new Promise<void>(resolve => this.tasks.push(resolve));
  }
  release() {
    this.active--;
    if (this.tasks.length > 0) {
      this.active++;
      const next = this.tasks.shift()!;
      next();
    }
  }
}

const PREVIEW_LIMIT = Math.max(1, Math.floor(navigator.hardwareConcurrency * 0.75));
const previewSemaphore = new AsyncSemaphore(PREVIEW_LIMIT);

function JobPreview({ path, type }: { path: string, type: "texture" | "dds" }) {
  const [preview, setPreview] = useState<string | null>(null);

  useEffect(() => {
    if (!path) return;
    let active = true;
    const cmd = type === "texture" ? "tauri_get_texture_preview" : "tauri_get_dds_preview";

    (async () => {
      await previewSemaphore.acquire();
      if (!active) {
        previewSemaphore.release();
        return;
      }
      try {
        const res = await invoke<string>(cmd, { path });
        if (active) setPreview(res);
      } catch {
        if (active) setPreview(null);
      } finally {
        previewSemaphore.release();
      }
    })();
    return () => { active = false; };
  }, [path, type]);

  if (!preview) return <div style={{ width: "100%", height: "100%", background: "var(--surface-2)", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-muted)", fontSize: "0.8rem", borderRadius: "6px" }}>...</div>;

  return <img src={`data:image/png;base64,${preview}`} style={{ width: "100%", height: "100%", objectFit: "contain", borderRadius: "6px", background: "var(--surface-2)" }} />;
}

function JobFormat({ path, type }: { path: string, type: "texture" | "dds" }) {
  const [format, setFormat] = useState<string>("...");
  useEffect(() => {
    let active = true;
    if (!path) { setFormat("N/A"); return; }
    const cmd = type === "texture" ? "tauri_get_texture_info" : "tauri_get_dds_info";
    invoke<TextureInfo>(cmd, { path }).then(info => {
      if (active) {
        const fmtStr = FORMAT_MAP[info.format] || `FORMAT_${info.format}`;
        setFormat(fmtStr.replace("DXGI_FORMAT_", ""));
      }
    }).catch(() => {
      if (active) setFormat("Err");
    });
    return () => { active = false; };
  }, [path, type]);

  return <span>{format}</span>;
}

interface TreeNode {
  name: string;
  path: string;
  isFolder: boolean;
  children: Map<string, TreeNode>;
}

function compactTree(node: TreeNode, isRoot: boolean = false) {
  if (!node.isFolder) return;
  for (const child of node.children.values()) {
    compactTree(child, false);
  }
  if (!isRoot && node.children.size === 1) {
    const singleChild = Array.from(node.children.values())[0];
    if (singleChild.isFolder) {
      node.name = `${node.name}/${singleChild.name}`;
      node.children = singleChild.children;
    }
  }
}

function buildTree(jobs: TextureJob[]): TreeNode {
  const root: TreeNode = { name: "", path: "", isFolder: true, children: new Map() };
  for (const job of jobs) {
    const parts = job.base_name.split("/");
    let current = root;
    let currentPath = "";
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      currentPath = currentPath ? `${currentPath}/${part}` : part;
      const isFolder = i < parts.length - 1;
      let child = current.children.get(part);
      if (!child) {
        child = { name: part, path: currentPath, isFolder, children: new Map() };
        current.children.set(part, child);
      }
      current = child;
    }
  }
  compactTree(root, true);
  return root;
}

function SimpleTree({ node, depth, onToggle, visibleJobs }: { node: TreeNode, depth: number, onToggle: (path: string, isFolder: boolean, node: TreeNode) => void, visibleJobs: Set<string> }) {
  const [open, setOpen] = useState(depth < 1);

  let isChecked = false;
  if (!node.isFolder) {
    isChecked = visibleJobs.has(node.path);
  } else {
    let allSelected = true;
    let anySelected = false;
    const checkAll = (n: TreeNode) => {
      if (!n.isFolder) {
        if (!visibleJobs.has(n.path)) allSelected = false;
        else anySelected = true;
      }
      n.children.forEach(checkAll);
    };
    checkAll(node);
    isChecked = allSelected && anySelected;
  }

  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", padding: "2px 0", paddingLeft: `${depth * 12 + (node.isFolder ? 0 : 20)}px`, cursor: "pointer" }} onClick={() => node.isFolder ? setOpen(!open) : onToggle(node.path, false, node)}>
        {node.isFolder ? (
          <span style={{ width: "16px", display: "inline-block", textAlign: "center", color: "var(--text-muted)" }}>{open ? "▾" : "▸"}</span>
        ) : (
          <span style={{ width: "16px", display: "inline-block" }}></span>
        )}
        <input type="checkbox" checked={isChecked} onChange={() => onToggle(node.path, node.isFolder, node)} onClick={(e) => e.stopPropagation()} style={{ marginRight: "6px" }} />
        <span style={{ fontSize: "0.85rem", color: node.isFolder ? "var(--text-primary)" : "var(--text-secondary)", wordBreak: "break-all" }}>{node.name}</span>
      </div>
      {node.isFolder && open && (
        <div>
          {Array.from(node.children.values())
            .sort((a, b) => {
              if (a.isFolder !== b.isFolder) return a.isFolder ? -1 : 1;
              return a.name.localeCompare(b.name);
            })
            .map(child => (
              <SimpleTree key={child.path} node={child} depth={depth + 1} onToggle={onToggle} visibleJobs={visibleJobs} />
            ))}
        </div>
      )}
    </div>
  );
}

function DdsDropzonePreview({ path, onDropPath }: { path: string | null, onDropPath: (path: string) => void }) {
  const [dragOver, setDragOver] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;

    getCurrentWindow().onDragDropEvent(event => {
      const payload = event.payload;
      if (payload.type === "leave") { setDragOver(false); return; }

      const el = containerRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      const inside = payload.position.x >= rect.left && payload.position.x <= rect.right && payload.position.y >= rect.top && payload.position.y <= rect.bottom;

      if (payload.type === "enter" || payload.type === "over") {
        setDragOver(inside);
        return;
      }

      setDragOver(false);
      if (!inside || payload.paths.length === 0) return;

      let droppedPath = payload.paths[0];
      if (droppedPath.startsWith("file://")) {
        try {
          const url = new URL(droppedPath);
          let p = decodeURIComponent(url.pathname);
          if (/^\/[A-Za-z]:\//.test(p)) p = p.slice(1);
          droppedPath = p.replace(/\//g, "\\");
        } catch { }
      }
      onDropPath(droppedPath);
    }).then(fn => {
      if (disposed) fn(); else unlisten = fn;
    });

    return () => { disposed = true; if (unlisten) unlisten(); };
  }, [onDropPath]);

  return (
    <div ref={containerRef} style={{ width: "100%", height: "100%", position: "relative" }}>
      {path ? <JobPreview path={path} type="dds" /> : <div style={{ width: "100%", height: "100%", background: "var(--surface-2)", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-muted)", fontSize: "0.75rem", borderRadius: "6px", textAlign: "center", padding: "0.5rem", border: "1px dashed var(--border)" }}>Drop DDS Here</div>}
      {dragOver && <div style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0, background: "rgba(0, 255, 0, 0.15)", border: "2px dashed #4ade80", borderRadius: "6px", zIndex: 10, pointerEvents: "none" }} />}
    </div>
  );
}

export default function TextureConverter() {
  const [tab, setTab] = useState<Tab>("extract");

  const [sourcePath, setSourcePath] = useState("");
  const [ddsPath, setDdsPath] = useState("");
  const [outDir, setOutDir] = useState("");

  const [projectDir, setProjectDir] = useState("");
  const [ddsDir, setDdsDir] = useState("");
  const [jobs, setJobs] = useState<TextureJob[]>([]);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [selectedProject, setSelectedProject] = useState("");
  const [projectSource, setProjectSource] = useState<"stager" | "custom">("stager");
  const [visibleJobs, setVisibleJobs] = useState<Set<string>>(new Set());

  const [showNewProject, setShowNewProject] = useState(false);
  const [newProjName, setNewProjName] = useState("");
  const [newProjAuthor, setNewProjAuthor] = useState("");
  const [creatingProject, setCreatingProject] = useState(false);

  useEffect(() => {
    invoke<ProjectInfo[]>("list_projects").then(setProjects).catch(console.error);
  }, []);

  useEffect(() => {
    if (projectSource === "stager") {
      if (selectedProject) {
        invoke<string>("get_project_path", { name: selectedProject })
          .then(setProjectDir)
          .catch(console.error);
      } else {
        setProjectDir("");
      }
    }
  }, [projectSource, selectedProject]);

  const [ignoreFormat, setIgnoreFormat] = useState(false);

  const [sourceInfo, setSourceInfo] = useState<TextureInfo | null>(null);
  const [ddsInfo, setDdsInfo] = useState<TextureInfo | null>(null);

  const [sourcePreview, setSourcePreview] = useState<string | null>(null);
  const [ddsPreview, setDdsPreview] = useState<string | null>(null);
  const [isSourcePreviewLoading, setIsSourcePreviewLoading] = useState(false);
  const [isDdsPreviewLoading, setIsDdsPreviewLoading] = useState(false);

  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function handleCreateProject() {
    if (!newProjName.trim()) return;
    setCreatingProject(true);
    try {
      await invoke("create_project", {
        name: newProjName.trim(),
        game: "RCRA",
        author: newProjAuthor.trim(),
      });
      const projs = await invoke<ProjectInfo[]>("list_projects");
      setProjects(projs);
      setSelectedProject(newProjName.trim());
      pushLog("success", `Project "${newProjName.trim()}" created.`);
      setShowNewProject(false);
      setNewProjName("");
      setNewProjAuthor("");
    } catch (e) {
      pushLog("error", `Failed to create project: ${e}`);
    } finally {
      setCreatingProject(false);
    }
  }

  useEffect(() => {
    if (tab !== "batch" || !projectDir) return;

    let cancelled = false;
    setRunning(true);
    setLog([]);
    pushLog("info", "Scanning project directory...");

    invoke<TextureJob[]>("tauri_scan_stager_textures", {
      projectDir,
      ddsDir: ddsDir || null,
    }).then(result => {
      if (cancelled) return;
      setJobs(result.map(j => ({ ...j, selected: !!j.sd_path && !!j.dds_path })));
      pushLog("success", `Found ${result.length} unique asset groups.`);
    }).catch(e => {
      if (cancelled) return;
      pushLog("error", String(e));
    }).finally(() => {
      if (!cancelled) setRunning(false);
    });

    return () => { cancelled = true; };
  }, [projectDir, ddsDir, tab]);

  const handleDropDds = (idx: number, path: string) => {
    setJobs(prev => {
      const copy = [...prev];
      copy[idx] = { ...copy[idx], dds_path: path, selected: true };
      return copy;
    });
  };

  const toggleVisible = (_path: string, isFolder: boolean, node: TreeNode) => {
    setVisibleJobs(prev => {
      const next = new Set(prev);
      const toggleNode = (n: TreeNode, forceAdd?: boolean) => {
        if (!n.isFolder) {
          if (forceAdd === undefined) {
            if (next.has(n.path)) next.delete(n.path);
            else next.add(n.path);
          } else {
            if (forceAdd) next.add(n.path);
            else next.delete(n.path);
          }
        } else {
          n.children.forEach(child => toggleNode(child, forceAdd));
        }
      };
      if (!isFolder) {
        toggleNode(node);
      } else {
        let allSelected = true;
        const checkAll = (n: TreeNode) => {
          if (!n.isFolder && !next.has(n.path)) allSelected = false;
          n.children.forEach(checkAll);
        };
        checkAll(node);
        toggleNode(node, !allSelected);
      }
      return next;
    });
  };

  const treeRoot = buildTree(jobs);
  treeRoot.name = projectSource === "stager" ? selectedProject : (projectDir.split(/[/\\]/).pop() || "Project");



  useEffect(() => {
    if (!sourcePath) {
      setSourceInfo(null);
      setSourcePreview(null);
      return;
    }

    setIsSourcePreviewLoading(true);

    invoke<TextureInfo>("tauri_get_texture_info", { path: sourcePath })
      .then(setSourceInfo)
      .catch((e) => {
        setSourceInfo(null);
        pushLog("error", `Failed to read source info: ${e}`);
      });

    invoke<string>("tauri_get_texture_preview", { path: sourcePath })
      .then((preview) => {
        setSourcePreview(preview);
        setIsSourcePreviewLoading(false);
      })
      .catch((e) => {
        setSourcePreview(null);
        setIsSourcePreviewLoading(false);
        console.warn("Failed to get preview", e);
      });
  }, [sourcePath]);

  useEffect(() => {
    if (!ddsPath) {
      setDdsInfo(null);
      setDdsPreview(null);
      return;
    }

    setIsDdsPreviewLoading(true);

    invoke<TextureInfo>("tauri_get_dds_info", { path: ddsPath })
      .then(setDdsInfo)
      .catch((e) => {
        setDdsInfo(null);
        pushLog("error", `Failed to read DDS info: ${e}`);
      });

    invoke<string>("tauri_get_dds_preview", { path: ddsPath })
      .then((preview) => {
        setDdsPreview(preview);
        setIsDdsPreviewLoading(false);
      })
      .catch((e) => {
        setDdsPreview(null);
        setIsDdsPreviewLoading(false);
        console.warn("Failed to get DDS preview", e);
      });
  }, [ddsPath]);

  async function handleBatchRun(mode: "replace" | "extract") {
    const selectedJobs = jobs.filter(j => j.sd_path && visibleJobs.has(j.base_name));
    if (selectedJobs.length === 0) {
      pushLog("error", "No valid jobs selected in the tree.");
      return;
    }

    let extractOutDir = outDir || null;
    if (mode === "extract") {
      const selectedDir = await open({ directory: true, title: "Select Output Folder for Extracted DDS Files" });
      if (typeof selectedDir !== "string") {
        return;
      }
      extractOutDir = selectedDir;
    }

    setRunning(true);
    setLog([]);
    try {
      if (mode === "extract") {
        pushLog("info", `Starting batch extraction of ${selectedJobs.length} textures...`);
        const result = await invoke<string>("tauri_batch_extract_textures", {
          jobs: selectedJobs,
          outputDir: extractOutDir,
          projectDir,
        });
        pushLog("success", result);
      } else {
        pushLog("info", `Starting batch replacement of ${selectedJobs.length} textures...`);
        const result = await invoke<string>("tauri_batch_replace_textures", {
          jobs: selectedJobs,
          outputDir: outDir || null,
          ignoreFormat,
          projectDir,
        });
        pushLog("success", result);
      }
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  async function handleRun() {
    if (!sourcePath) {
      pushLog("error", "Select a source .texture file first.");
      return;
    }
    if (tab === "replace" && !ddsPath) {
      pushLog("error", "Select a .dds file to inject.");
      return;
    }

    setRunning(true);
    setLog([]);

    try {
      let result: string;
      if (tab === "extract") {
        pushLog("info", `Extracting texture from ${sourcePath}...`);
        result = await invoke("tauri_extract_texture", {
          sourcePath,
          outputDir: outDir || null,
        });
      } else {
        pushLog("info", `Injecting ${ddsPath} into ${sourcePath}...`);
        result = await invoke("tauri_replace_texture", {
          sourcePath,
          ddsPath,
          outputDir: outDir || null,
          ignoreFormat,
        });
      }
      pushLog("success", `Operation successful:\n${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>Texture Converter</h2>
      <p className={styles.subtitle}>Extract or replace texture formats</p>

      <div className={styles.tabs}>
        <button
          className={`${styles.tab} ${tab === "extract" ? styles.active : ""}`}
          onClick={() => setTab("extract")}
        >
          Extract
        </button>
        <button
          className={`${styles.tab} ${tab === "replace" ? styles.active : ""}`}
          onClick={() => setTab("replace")}
        >
          Replace
        </button>
        <button
          className={`${styles.tab} ${tab === "batch" ? styles.active : ""}`}
          onClick={() => setTab("batch")}
        >
          Batch Convert
        </button>
      </div>

      <div className={styles.panel}>
        <div style={{ display: "flex", gap: "2rem", flex: 1, minHeight: 0 }}>
          <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: "1rem", minHeight: 0 }}>
            {tab !== "batch" ? (
              <>
                <FilePickerInput
                  label="Source .texture"
                  value={sourcePath}
                  onChange={setSourcePath}
                  mode="open"
                  filters={TEXTURE_FILTER}
                />

                {tab === "replace" && (
                  <FilePickerInput
                    label="DDS Texture"
                    value={ddsPath}
                    onChange={setDdsPath}
                    mode="open"
                    filters={DDS_FILTER}
                  />
                )}

                <FilePickerInput
                  label="Output Directory (Optional)"
                  value={outDir}
                  onChange={setOutDir}
                  mode="dir"
                />

                {tab === "replace" && (
                  <div style={{ display: "flex", gap: "1.5rem", flexWrap: "wrap", marginTop: "0.5rem" }}>
                    <label className={styles.checkboxLabel}>
                      <input
                        type="checkbox"
                        checked={ignoreFormat}
                        onChange={(e) => setIgnoreFormat(e.target.checked)}
                      />
                      Ignore DXGI format mismatches
                    </label>
                  </div>
                )}

                <div style={{ display: "flex", gap: "1rem" }}>
                  <button className={styles.runBtn} onClick={handleRun} disabled={running}>
                    {running ? "Processing..." : tab === "extract" ? "Extract Texture" : "Replace Texture"}
                  </button>
                </div>

                <div className={styles.tableWrap}>
                  <table className={styles.table}>
                    <thead>
                      <tr>
                        <th></th>
                        <th>Width</th>
                        <th>Height</th>
                        <th>Mipmaps</th>
                        <th>HDMipmaps</th>
                        <th>Images</th>
                        <th>BytesPerPixel</th>
                        <th>Size</th>
                        <th>HDSize</th>
                        <th>Format</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr>
                        <td><strong>Source</strong></td>
                        {sourceInfo ? (
                          <>
                            <td>{sourceInfo.width}</td>
                            <td>{sourceInfo.height}</td>
                            <td>{sourceInfo.mipmaps}</td>
                            <td>{sourceInfo.hdmipmaps}</td>
                            <td>{sourceInfo.images}</td>
                            <td>{sourceInfo.bytes_per_pixel}</td>
                            <td>{sourceInfo.size}</td>
                            <td>{sourceInfo.hdsize}</td>
                            <td>{FORMAT_MAP[sourceInfo.format] || `DXGI_FORMAT_${sourceInfo.format}`}</td>
                          </>
                        ) : (
                          <td colSpan={9} style={{ color: "var(--text-secondary)" }}>No source file selected</td>
                        )}
                      </tr>
                      {tab === "replace" && (
                        <tr>
                          <td><strong>Custom</strong></td>
                          {ddsInfo ? (
                            <>
                              <td>{ddsInfo.width}</td>
                              <td>{ddsInfo.height}</td>
                              <td>{ddsInfo.mipmaps}</td>
                              <td>{ddsInfo.hdmipmaps}</td>
                              <td>{ddsInfo.images}</td>
                              <td>{ddsInfo.bytes_per_pixel}</td>
                              <td>{ddsInfo.size}</td>
                              <td>{ddsInfo.hdsize}</td>
                              <td>{FORMAT_MAP[ddsInfo.format] || `DXGI_FORMAT_${ddsInfo.format}`}</td>
                            </>
                          ) : (
                            <td colSpan={9} style={{ color: "var(--text-secondary)" }}>No DDS file selected</td>
                          )}
                        </tr>
                      )}
                    </tbody>
                  </table>
                </div>
              </>
            ) : (
              <div style={{ display: "flex", gap: "1.5rem", flex: 1, minHeight: 0 }}>
                <div style={{ width: "320px", minWidth: "320px", display: "flex", flexDirection: "column", gap: "1rem", overflowY: "auto", paddingRight: "0.5rem" }}>
                  <div style={{ display: "flex", flexDirection: "column", gap: "0.4rem" }}>
                    <label style={{ fontSize: "0.875rem", fontWeight: 500, color: "var(--text-primary)" }}>Textures Source</label>
                    <div style={{ display: "flex", gap: "1rem" }}>
                      <label className={styles.checkboxLabel}>
                        <input type="radio" checked={projectSource === "stager"} onChange={() => setProjectSource("stager")} />
                        Stager Project
                      </label>
                      <label className={styles.checkboxLabel}>
                        <input type="radio" checked={projectSource === "custom"} onChange={() => setProjectSource("custom")} />
                        Custom Folder
                      </label>
                    </div>
                  </div>

                  {projectSource === "stager" ? (
                    <div style={{ display: "flex", flexDirection: "column", gap: "0.4rem" }}>
                      {showNewProject ? (
                        <div className={styles.newProjectForm}>
                          <input
                            className={styles.newProjInputFull}
                            placeholder="Project name"
                            value={newProjName}
                            onChange={(e) => setNewProjName(e.target.value)}
                            onKeyDown={(e) => e.key === "Enter" && handleCreateProject()}
                            autoFocus
                          />
                          <input
                            className={styles.newProjInputFull}
                            placeholder="Author"
                            value={newProjAuthor}
                            onChange={(e) => setNewProjAuthor(e.target.value)}
                            onKeyDown={(e) => e.key === "Enter" && handleCreateProject()}
                          />
                          <div className={styles.formActions}>
                            <button
                              className={styles.runBtn}
                              style={{ padding: "0.4rem", flex: 1 }}
                              onClick={handleCreateProject}
                              disabled={creatingProject || !newProjName.trim()}
                            >
                              {creatingProject ? "Creating…" : "Create"}
                            </button>
                            <button
                              className={styles.cancelProjBtn}
                              onClick={() => { setShowNewProject(false); setNewProjName(""); setNewProjAuthor(""); }}
                            >
                              Cancel
                            </button>
                          </div>
                        </div>
                      ) : (
                        <div className={styles.projectPickerRow}>
                          <select
                            className={styles.projectSelectFull}
                            value={selectedProject}
                            onChange={(e) => setSelectedProject(e.target.value)}
                          >
                            {projects.length === 0 && <option value="">No projects</option>}
                            {projects.map((p) => (
                              <option key={p.name} value={p.name}>{p.name}</option>
                            ))}
                          </select>
                          <button
                            className={styles.newProjBtnSmall}
                            onClick={() => setShowNewProject(true)}
                            title="Create a new project"
                          >
                            + New
                          </button>
                        </div>
                      )}
                    </div>
                  ) : (
                    <FilePickerInput
                      label="Custom Project Folder"
                      value={projectDir}
                      onChange={setProjectDir}
                      mode="dir"
                    />
                  )}

                  <FilePickerInput
                    label="DDS Source Folder (Optional)"
                    value={ddsDir}
                    onChange={setDdsDir}
                    mode="dir"
                  />

                  <FilePickerInput
                    label="Output Directory (Optional)"
                    value={outDir}
                    onChange={setOutDir}
                    mode="dir"
                  />

                  <label className={styles.checkboxLabel}>
                    <input
                      type="checkbox"
                      checked={ignoreFormat}
                      onChange={(e) => setIgnoreFormat(e.target.checked)}
                    />
                    Ignore DXGI format mismatches
                  </label>

                  <div style={{ display: "flex", gap: "0.5rem" }}>
                    <button className={styles.runBtn} style={{ background: "var(--surface-3)", color: "var(--text-primary)" }} onClick={() => handleBatchRun("extract")} disabled={running || jobs.length === 0}>
                      {running ? "Processing..." : "Batch Extract"}
                    </button>
                    <button className={styles.runBtn} onClick={() => handleBatchRun("replace")} disabled={running || jobs.length === 0}>
                      {running ? "Processing..." : "Batch Replace"}
                    </button>
                  </div>

                  <div style={{ flex: 1, border: "1px solid var(--border)", borderRadius: "8px", background: "var(--surface-1)", display: "flex", flexDirection: "column", minHeight: 0, marginTop: "0.5rem" }}>
                    <div style={{ padding: "0.5rem", fontWeight: 600, borderBottom: "1px solid var(--border)", background: "var(--surface-2)" }}>Project Textures</div>
                    <div style={{ padding: "0.5rem", overflowY: "auto", flex: 1 }}>
                      <SimpleTree node={treeRoot} depth={0} onToggle={toggleVisible} visibleJobs={visibleJobs} />
                    </div>
                  </div>
                </div>

                <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: "1rem", overflowY: "auto", paddingRight: "0.5rem" }}>
                  {jobs.length > 0 ? (
                    <>
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                        <div style={{ fontSize: "0.9rem", color: "var(--text-secondary)" }}>
                          Select the textures you want to replace:
                        </div>
                        <label className={styles.checkboxLabel} style={{ fontSize: "0.85rem" }}>
                          <input
                            type="checkbox"
                            checked={jobs.length > 0 && jobs.every(j => j.selected)}
                            onChange={(e) => setJobs(jobs.map(j => ({ ...j, selected: j.sd_path && j.dds_path ? e.target.checked : false })))}
                          />
                          Select All Valid
                        </label>
                      </div>
                      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))", gap: "1rem" }}>
                        {jobs.map((job, idx) => {
                          if (!visibleJobs.has(job.base_name)) return null;

                          const sdOk = !!job.sd_path;
                          const ddsOk = !!job.dds_path;
                          const hasBoth = sdOk && ddsOk;

                          return (
                            <div key={idx} style={{
                              border: `2px solid ${job.selected ? "var(--accent)" : "var(--border)"}`,
                              borderRadius: "8px",
                              padding: "0.75rem",
                              background: job.selected ? "var(--surface-2)" : "var(--surface-1)",
                              opacity: hasBoth ? 1 : 0.8,
                              cursor: hasBoth ? "pointer" : "default",
                              display: "flex",
                              flexDirection: "column",
                              gap: "0.75rem",
                              position: "relative",
                              transition: "all 0.15s ease"
                            }} onClick={() => {
                              if (hasBoth) {
                                const newJobs = [...jobs];
                                newJobs[idx].selected = !newJobs[idx].selected;
                                setJobs(newJobs);
                              }
                            }}>
                              {job.selected && <div style={{ position: "absolute", top: "0.5rem", right: "0.5rem", width: "12px", height: "12px", borderRadius: "50%", background: "var(--accent)", zIndex: 10 }} />}

                              <div style={{ height: "120px", display: "flex", gap: "0.5rem", width: "100%" }}>
                                <div style={{ flex: 1, height: "100%", position: "relative" }}>
                                  {job.sd_path && <JobPreview path={job.sd_path} type="texture" />}
                                  <div style={{ position: "absolute", bottom: 0, left: 0, background: "rgba(0,0,0,0.7)", color: "#fff", fontSize: "0.6rem", padding: "0.1rem 0.3rem", borderTopRightRadius: "4px", pointerEvents: "none" }}>Original</div>
                                </div>
                                <div style={{ flex: 1, height: "100%", position: "relative" }} onClick={e => e.stopPropagation()}>
                                  <DdsDropzonePreview path={job.dds_path} onDropPath={(path) => handleDropDds(idx, path)} />
                                  <div style={{ position: "absolute", bottom: 0, right: 0, background: "rgba(0,0,0,0.7)", color: "#fff", fontSize: "0.6rem", padding: "0.1rem 0.3rem", borderTopLeftRadius: "4px", pointerEvents: "none" }}>New</div>
                                </div>
                              </div>

                              <div style={{ fontSize: "0.85rem", fontWeight: 600, wordBreak: "break-all", lineHeight: 1.2 }}>{job.base_name.split("/").pop()}</div>

                              <div style={{ fontSize: "0.75rem", color: "var(--text-secondary)", display: "flex", justifyContent: "space-between" }}>
                                <div style={{ color: "var(--text-tertiary)", fontWeight: 700, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", paddingRight: "0.5rem" }} title="DXGI Format">
                                  {job.sd_path ? <JobFormat path={job.sd_path} type="texture" /> : "Unknown"}
                                </div>
                                <div style={{ color: ddsOk ? "var(--accent)" : "inherit" }}>DDS: {ddsOk ? "Ready" : "Missing"}</div>
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    </>
                  ) : (
                    <div style={{ padding: "2rem", textAlign: "center", color: "var(--text-secondary)" }}>
                      Scan a Stager project to see discovered textures.
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>

          {(sourcePreview || isSourcePreviewLoading || (tab === "replace" && (ddsPreview || isDdsPreviewLoading))) && (
            <div style={{ width: "160px", display: "flex", flexDirection: "column", gap: "1rem" }}>
              {(sourcePreview || isSourcePreviewLoading) && (
                <div style={{ textAlign: "center" }}>
                  {isSourcePreviewLoading ? (
                    <div style={{ width: "100%", aspectRatio: "1", borderRadius: "6px", background: "var(--bg-base)", border: "1px solid var(--border)", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-muted)", fontSize: "0.85rem" }}>
                      Loading...
                    </div>
                  ) : (
                    <img src={`data:image/png;base64,${sourcePreview}`} style={{ width: "100%", borderRadius: "6px", objectFit: "contain", background: "var(--bg-base)", border: "1px solid var(--border)" }} />
                  )}
                  <div style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginTop: "0.25rem" }}>Source</div>
                </div>
              )}
              {tab === "replace" && (ddsPreview || isDdsPreviewLoading) && (
                <div style={{ textAlign: "center" }}>
                  {isDdsPreviewLoading ? (
                    <div style={{ width: "100%", aspectRatio: "1", borderRadius: "6px", background: "var(--bg-base)", border: "1px solid var(--border)", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-muted)", fontSize: "0.85rem" }}>
                      Loading...
                    </div>
                  ) : (
                    <img src={`data:image/png;base64,${ddsPreview}`} style={{ width: "100%", borderRadius: "6px", objectFit: "contain", background: "var(--bg-base)", border: "1px solid var(--border)" }} />
                  )}
                  <div style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginTop: "0.25rem" }}>Custom</div>
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      <StatusLog entries={log} />
    </div>
  );
}

