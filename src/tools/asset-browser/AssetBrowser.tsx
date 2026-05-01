import { useState, useCallback, useRef, useDeferredValue, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { useNavigate } from "react-router-dom";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import TreeView, { type TreeNodeData } from "./TreeView";
import { useSettings } from "../../contexts/SettingsContext";
import styles from "./AssetBrowser.module.css";

const SEND_TO_ROUTES: Record<string, { label: string; route: string }> = {
  config:   { label: "Config Editor",     route: "/tools/config-editor" },
  actor:    { label: "Config Editor",     route: "/tools/config-editor" },
  conduit:  { label: "Config Editor",     route: "/tools/config-editor" },
  performanceset: { label: "Config Editor", route: "/tools/config-editor" },
  model:    { label: "Model Converter",   route: "/tools/model-converter" },
  material: { label: "Material Remapper", route: "/tools/material-remapper" },
};

function extOf(path: string) {
  return path.split(".").pop()?.toLowerCase() ?? "";
}

interface TocInfo {
  asset_count: number;
  archive_count: number;
  archive_names: string[];
  span_count: number;
}

interface AssetInfo {
  id: string;
  archive_index: number;
  offset: number;
  size: number;
  span: number;
}

interface ProjectInfo {
  name: string;
  game: string;
  author: string;
}

function buildTree(
  assets: AssetInfo[],
  hashMap: Map<string, string>,
  archiveNames: string[],
): TreeNodeData {
  const root: TreeNodeData = { name: "", fullPath: "", children: new Map() };

  function ensurePath(parts: string[]): TreeNodeData {
    let current = root;
    let fullPath = "";
    for (const part of parts) {
      fullPath = fullPath ? `${fullPath}/${part}` : part;
      let child = current.children.get(part);
      if (!child) {
        child = { name: part, fullPath, children: new Map() };
        current.children.set(part, child);
      }
      current = child;
    }
    return current;
  }

  for (const asset of assets) {
    const resolvedPath = hashMap.get(asset.id);
    let parts: string[];

    if (resolvedPath) {
      parts = resolvedPath.split("/").filter(Boolean);
    } else {
      const archiveName = archiveNames[asset.archive_index] ?? `archive_${asset.archive_index}`;
      parts = ["[UNKNOWN]", archiveName, asset.id];
    }

    const node = ensurePath(parts);
    const spanEntry = { span: asset.span, size: asset.size, archiveIndex: asset.archive_index };
    if (node.asset) {
      // Same asset ID appearing in a different span (e.g. SD→HD texture pair) — merge.
      node.asset.spans.push(spanEntry);
      node.asset.spans.sort((a, b) => a.span - b.span);
    } else {
      node.asset = { id: asset.id, spans: [spanEntry] };
    }
  }

  return root;
}

// Builds a flat DFS leaf-order list and a path→node map for range-select and extraction.
function buildAssetIndex(root: TreeNodeData): {
  assetMap: Map<string, TreeNodeData>;
  flatLeafOrder: string[];
} {
  const assetMap = new Map<string, TreeNodeData>();
  const flatLeafOrder: string[] = [];

  function traverse(node: TreeNodeData) {
    if (node.asset) {
      assetMap.set(node.fullPath, node);
      flatLeafOrder.push(node.fullPath);
    }
    const sorted = Array.from(node.children.values()).sort((a, b) => {
      const aF = a.children.size > 0;
      const bF = b.children.size > 0;
      if (aF !== bF) return aF ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    for (const child of sorted) traverse(child);
  }

  traverse(root);
  return { assetMap, flatLeafOrder };
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

export default function AssetBrowser() {
  const { settings } = useSettings();
  const navigate = useNavigate();
  const archivesDir = settings.archivesDir;
  const [loading, setLoading] = useState(false);
  const [tocInfo, setTocInfo] = useState<TocInfo | null>(null);
  const [tree, setTree] = useState<TreeNodeData | null>(null);
  const [hashCount, setHashCount] = useState(0);

  // Multi-select state
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const lastClickedPathRef = useRef<string | null>(null);
  const assetMapRef = useRef<Map<string, TreeNodeData>>(new Map());
  const flatLeafOrderRef = useRef<string[]>([]);

  const [filter, setFilter] = useState("");
  const deferredFilter = useDeferredValue(filter);

  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [selectedProject, setSelectedProject] = useState("");
  const [extracting, setExtracting] = useState(false);

  // New-project inline form
  const [showNewProject, setShowNewProject] = useState(false);
  const [newProjName, setNewProjName] = useState("");
  const [newProjAuthor, setNewProjAuthor] = useState("");
  const [creatingProject, setCreatingProject] = useState(false);

  const [log, setLog] = useState<LogEntry[]>([]);
  const tocPathRef = useRef("");

  // Context menu
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; node: TreeNodeData } | null>(null);
  const [sendToStager, setSendToStager] = useState<{ file: string; defaultPath: string } | null>(null);

  useEffect(() => {
    const dismiss = () => setCtxMenu(null);
    window.addEventListener("click", dismiss);
    return () => window.removeEventListener("click", dismiss);
  }, []);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  const refreshProjects = useCallback(async () => {
    try {
      const projectList: ProjectInfo[] = await invoke("list_projects");
      setProjects(projectList);
      setSelectedProject((prev) => {
        if (projectList.length === 0) return "";
        if (prev && projectList.some((p) => p.name === prev)) return prev;
        return projectList[0].name;
      });
    } catch (e) {
      pushLog("error", `Failed to refresh projects: ${e}`);
    }
  }, []);

  useEffect(() => {
    const unlisten = listen("projects-changed", () => {
      refreshProjects();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refreshProjects]);

  const handleLoad = useCallback(async () => {
    if (!archivesDir) {
      pushLog("error", "Select an archives directory first.");
      return;
    }

    setLoading(true);
    setLog([]);
    setTree(null);
    setTocInfo(null);
    setSelectedPaths(new Set());
    lastClickedPathRef.current = null;

    try {
      const tocPath = `${archivesDir}\\toc`;
      tocPathRef.current = tocPath;

      pushLog("info", "Loading TOC…");
      const info: TocInfo = await invoke("load_toc", { tocPath });
      setTocInfo(info);
      pushLog("success", `TOC loaded: ${info.asset_count} assets, ${info.archive_count} archives`);

      pushLog("info", "Loading hashes…");
      let hashMap = new Map<string, string>();
      try {
        const pairs: [string, string][] = await invoke("load_hashes");
        for (const [hex, path] of pairs) {
          hashMap.set(hex, path);
        }
        setHashCount(hashMap.size);
        pushLog("success", `Loaded ${hashMap.size} hashes`);
      } catch (e) {
        pushLog("warning", `Could not load hashes — all assets will be [UNKNOWN]. (${e})`);
      }

      pushLog("info", "Listing assets…");
      const assets: AssetInfo[] = await invoke("list_toc_assets", { tocPath });

      const treeRoot = buildTree(assets, hashMap, info.archive_names);
      const { assetMap, flatLeafOrder } = buildAssetIndex(treeRoot);
      assetMapRef.current = assetMap;
      flatLeafOrderRef.current = flatLeafOrder;

      setTree(treeRoot);
      pushLog("success", `Tree built with ${assets.length} assets`);

      await refreshProjects();
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setLoading(false);
    }
  }, [archivesDir, refreshProjects]);

  const handleSelect = useCallback((node: TreeNodeData, event: React.MouseEvent) => {
    if (!node.asset) return;
    const path = node.fullPath;

    if (event.ctrlKey || event.metaKey) {
      setSelectedPaths((prev) => {
        const next = new Set(prev);
        if (next.has(path)) next.delete(path);
        else next.add(path);
        return next;
      });
      lastClickedPathRef.current = path;
    } else if (event.shiftKey && lastClickedPathRef.current) {
      const order = flatLeafOrderRef.current;
      const a = order.indexOf(lastClickedPathRef.current);
      const b = order.indexOf(path);
      if (a !== -1 && b !== -1) {
        const [lo, hi] = a <= b ? [a, b] : [b, a];
        setSelectedPaths(new Set(order.slice(lo, hi + 1)));
      } else {
        setSelectedPaths(new Set([path]));
        lastClickedPathRef.current = path;
      }
    } else {
      setSelectedPaths(new Set([path]));
      lastClickedPathRef.current = path;
    }
  }, []);

  async function handleExtract() {
    if (selectedPaths.size === 0 || !selectedProject || !tocPathRef.current) return;

    setExtracting(true);
    let ok = 0;
    try {
      for (const path of selectedPaths) {
        const node = assetMapRef.current.get(path);
        if (!node?.asset) continue;
        try {
          const result: string = await invoke("extract_asset_to_project", {
            tocPath: tocPathRef.current,
            assetId: node.asset.id,
            archivesDir,
            projectName: selectedProject,
            assetPath: path,
          });
          pushLog("success", `→ ${path}: ${result}`);
          ok++;
        } catch (e) {
          pushLog("error", `✗ ${path}: ${e}`);
        }
      }
      if (selectedPaths.size > 1) {
        pushLog(ok > 0 ? "success" : "error", `Extracted ${ok}/${selectedPaths.size} assets to "${selectedProject}"`);
      }
    } finally {
      setExtracting(false);
    }
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
      await refreshProjects();
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

  const handleContextMenu = useCallback((node: TreeNodeData, e: React.MouseEvent) => {
    e.preventDefault();
    setCtxMenu({ x: e.clientX, y: e.clientY, node });
  }, []);

  async function handleSendToTool(node: TreeNodeData, route: string) {
    setCtxMenu(null);
    if (!node.asset || !tocPathRef.current || !archivesDir) return;
    try {
      pushLog("info", `Extracting ${node.fullPath} to temp…`);
      const tempPath: string = await invoke("extract_to_temp", {
        tocPath: tocPathRef.current,
        assetId: node.asset.id,
        archivesDir,
        filename: node.fullPath,
      });
      navigate(route, { state: { filePath: tempPath, assetPath: node.fullPath } });
    } catch (e) {
      pushLog("error", `Failed to extract: ${e}`);
    }
  }

  async function handleExtractAssetToPath(node: TreeNodeData) {
    setCtxMenu(null);
    if (!node.asset || !tocPathRef.current || !archivesDir) return;

    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Extract Asset To Folder",
      });

      if (!selected || Array.isArray(selected)) return;

      const result: string = await invoke("extract_asset_to_path", {
        tocPath: tocPathRef.current,
        assetId: node.asset.id,
        archivesDir,
        outputDir: selected,
        assetPath: node.fullPath,
      });
      pushLog("success", `Extracted ${node.fullPath} → ${result}`);
    } catch (e) {
      pushLog("error", `Extract to path failed: ${e}`);
    }
  }

  async function handleCopyAssetPath(node: TreeNodeData) {
    setCtxMenu(null);
    try {
      await navigator.clipboard.writeText(node.fullPath);
      pushLog("success", `Copied asset path: ${node.fullPath}`);
    } catch (e) {
      pushLog("error", `Copy path failed: ${e}`);
    }
  }

  // Derived summary for selection
  const selectionCount = selectedPaths.size;
  const singleNode = selectionCount === 1 ? assetMapRef.current.get([...selectedPaths][0]) : null;
  const totalSize = selectionCount > 1
    ? [...selectedPaths].reduce((sum, p) => {
      const n = assetMapRef.current.get(p);
      return sum + (n?.asset?.spans.reduce((s, sp) => s + sp.size, 0) ?? 0);
    }, 0)
    : 0;

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <div className={styles.headerTitle}>
          <h2 className={styles.title}>Asset Browser</h2>
          <p className={styles.subtitle}>Browse and extract game assets</p>
        </div>
        <button className={styles.loadBtn} onClick={handleLoad} disabled={loading || !archivesDir}>
          {loading ? "Loading…" : "Load"}
        </button>
      </div>

      <div className={styles.mainContent}>
        {/* Left Column: Tree */}
        <div className={styles.treeColumn}>
          {/* Stats Bar */}
          {tocInfo && (
            <div className={styles.statsBar}>
              <span className={styles.statItem}>
                Assets: <span className={styles.statValue}>{tocInfo.asset_count.toLocaleString()}</span>
              </span>
              <span className={styles.statItem}>
                Archives: <span className={styles.statValue}>{tocInfo.archive_count}</span>
              </span>
              <span className={styles.statItem}>
                Hashes: <span className={styles.statValue}>{hashCount.toLocaleString()}</span>
              </span>
            </div>
          )}

          {/* Search + Tree */}
          {tree ? (
            <>
              <div className={styles.searchBar}>
                <input
                  className={styles.searchInput}
                  type="text"
                  placeholder="Filter assets by path…"
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                />
              </div>
              <div className={styles.treeContainer}>
                <TreeView
                  root={tree}
                  selectedPaths={selectedPaths}
                  onSelect={handleSelect}
                  onContextMenu={handleContextMenu}
                  filter={deferredFilter}
                />
              </div>
            </>
          ) : (
            !loading && (
              <div className={styles.placeholder}>
                Select an archives folder and click Load to browse assets.
              </div>
            )
          )}
        </div>

        {/* Right Column: Details */}
        <div className={`${styles.detailsColumn} ${selectionCount > 0 ? styles.hasSelection : ""}`}>
          {selectionCount > 0 ? (
            <div className={styles.detailsContent}>
              <h3 className={styles.detailsTitle}>Selection Details</h3>
              
              <div className={styles.selectedInfoVertical}>
                {singleNode?.asset ? (
                  <>
                    <div className={styles.detailGroup}>
                      <label>Path</label>
                      <span className={styles.detailValuePath}>{singleNode.fullPath}</span>
                    </div>
                    <div className={styles.detailGroup}>
                      <label>Asset ID</label>
                      <span className={styles.detailValueMono}>{singleNode.asset.id}</span>
                    </div>
                    <div className={styles.detailGroup}>
                      <label>Spans</label>
                      <div className={styles.spansList}>
                        {singleNode.asset.spans.map((s, idx) => {
                          const isTexture = singleNode.fullPath.toLowerCase().endsWith(".texture");
                          const label = isTexture 
                            ? (s.span === 0 ? "SD" : s.span === 1 ? "HD" : `S${s.span}`) 
                            : `S${s.span}`;
                          return (
                            <div key={idx} className={styles.spanItem}>
                              <span className={styles.spanTag}>{label}</span>
                              <span className={styles.spanSize}>{formatSize(s.size)}</span>
                              <span className={styles.spanArc}>Arc #{s.archiveIndex}</span>
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  </>
                ) : (
                  <>
                    <div className={styles.detailGroup}>
                      <label>Selection</label>
                      <span className={styles.detailValue}>{selectionCount} assets</span>
                    </div>
                    <div className={styles.detailGroup}>
                      <label>Total Size</label>
                      <span className={styles.detailValue}>{formatSize(totalSize)}</span>
                    </div>
                  </>
                )}
              </div>

              <div className={styles.extractSection}>
                <h3 className={styles.detailsTitle}>Extract to Project</h3>
                <div className={styles.extractControlsVertical}>
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
                          className={styles.extractBtn}
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
                    <>
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
                      <button
                        className={styles.extractBtnFull}
                        onClick={handleExtract}
                        disabled={extracting || !selectedProject}
                      >
                        {extracting
                          ? "Extracting…"
                          : selectionCount > 1
                            ? `Extract ${selectionCount} Assets`
                            : "Extract Asset"}
                      </button>
                    </>
                  )}
                </div>
              </div>
            </div>
          ) : (
            <div className={styles.detailsPlaceholder}>
              Select assets in the tree to see details.
            </div>
          )}
        </div>
      </div>

      {/* Log */}
      <div className={styles.logContainer}>
        <StatusLog entries={log} />
      </div>

      {ctxMenu && (() => {
        const ext = extOf(ctxMenu.node.fullPath);
        const targets = Object.entries(SEND_TO_ROUTES).filter(([e]) => e === ext);
        return (
          <div
            style={{
              position: "fixed", top: ctxMenu.y, left: ctxMenu.x,
              background: "var(--bg-elevated)", border: "1px solid var(--border)",
              boxShadow: "0 4px 12px rgba(0,0,0,0.5)", borderRadius: "6px",
              padding: "4px 0", zIndex: 10000, minWidth: "190px",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <>
              <div className={styles.ctxItem} onClick={() => handleExtractAssetToPath(ctxMenu.node)}>
                Extract to Folder
              </div>
              <div className={styles.ctxItem} onClick={() => handleCopyAssetPath(ctxMenu.node)}>
                Copy Asset Path
              </div>
              {targets.length > 0 && (
                <>
                  <div style={{ height: 1, background: "var(--border)", margin: "4px 0" }} />
                <div style={{ padding: "3px 16px 2px", fontSize: "0.68rem", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.07em", color: "var(--text-muted)" }}>
                  Send To
                </div>
                {targets.map(([, t]) => (
                  <div key={t.route} className={styles.ctxItem} onClick={() => handleSendToTool(ctxMenu.node, t.route)}>
                    {t.label}
                  </div>
                ))}
                <div
                  className={styles.ctxItem}
                  onClick={async () => {
                    setCtxMenu(null);
                    if (!ctxMenu.node.asset || !tocPathRef.current || !archivesDir) return;
                    try {
                      const tempPath: string = await invoke("extract_to_temp", {
                        tocPath: tocPathRef.current,
                        assetId: ctxMenu.node.asset.id,
                        archivesDir,
                        filename: ctxMenu.node.fullPath,
                      });
                      setSendToStager({ file: tempPath, defaultPath: `0/${ctxMenu.node.fullPath}` });
                    } catch (e) {
                      pushLog("error", `Extract failed: ${e}`);
                    }
                  }}
                >
                  Stager (extract)
                </div>
                </>
              )}
              {targets.length === 0 && (
                <div style={{ padding: "6px 16px", fontSize: "0.78rem", color: "var(--text-muted)" }}>
                  No send-to tools for .{ext}
                </div>
              )}
            </>
          </div>
        );
      })()}

      {sendToStager && (
        <SendToStagerModal
          sourceFile={sendToStager.file}
          defaultTargetPath={sendToStager.defaultPath}
          onClose={() => setSendToStager(null)}
          onSent={(proj) => pushLog("success", `Sent to project "${proj}"`)}
        />
      )}
    </div>
  );
}
