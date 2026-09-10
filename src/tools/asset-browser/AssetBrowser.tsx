import { useState, useCallback, useRef, useDeferredValue, useEffect } from "react";
import { useProjects } from "../../contexts/ProjectsContext";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import AssetReferencesModal from "../../components/shared/AssetReferencesModal";
import TreeView, { type TreeNodeData, compareNodes } from "./TreeView";
import { useSettings } from "../../contexts/SettingsContext";
import { openToolWindow } from "../../utils/openToolWindow";
import { SEND_TO_ROUTES } from "../../utils/sendToRoutes";
import styles from "./AssetBrowser.module.css";
import { TocTextureViewer } from "../../components/shared/TextureViewer";
import { TocSoundbankViewer } from "../../components/shared/SoundbankViewer";
import { TocWemViewer } from "../../components/shared/WemViewer";

function extOf(path: string) {
  return path.split(".").pop()?.toLowerCase() ?? "";
}

/// Real asset path for a node — mods-branch nodes are display copies whose
/// `fullPath` is prefixed with `[MODS]/<archive>`.
function assetPathOf(node: TreeNodeData): string {
  return node.canonicalPath ?? node.fullPath;
}

const LANGUAGE_CODES = new Set([
  "us", "gb", "dk", "nl", "fi", "fr", "de", "it", "jp", "kr", "no", "pl", "pt", "ru", "es", "se",
  "br", "ar", "tr", "la", "cs", "ct", "fc", "cz", "hu", "el", "ro", "th", "vi", "id", "hr"
]);

function resolveWwisePath(assetId: string, archiveName: string): string | null {
  const cleanId = assetId.toUpperCase();
  if (cleanId.startsWith("E0000000") && cleanId.length === 16) {
    const hexPart = cleanId.substring(8);
    const wemId = parseInt(hexPart, 16);
    if (!isNaN(wemId)) {
      const normalizedArchive = archiveName.replace(/\\/g, "/");
      const match = normalizedArchive.match(/\.([a-z]{2})$/i);
      if (match) {
        const lang = match[1].toLowerCase();
        if (LANGUAGE_CODES.has(lang)) {
          return `sound/streamed/${lang}/${wemId}.wem`;
        }
      }
      return `sound/streamed/${wemId}.wem`;
    }
  }
  return null;
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

interface ModHashEntry {
  asset_id: string;
  path: string;
  span: number;
  mod_file: string;
  mod_archive: string | null;
}

interface ModHashResult {
  entries: ModHashEntry[];
  mods_read: number;
  installed: string[];
  profile: string | null;
  notes: string[];
}


// Overstrike's profile records the checkbox state at the time it was saved,
// not what was last written into the game. The TOC is the ground truth: match
// each .stage against the ids actually present in each `d\mods\modN` archive.
function verifyModArchives(
  assets: AssetInfo[],
  archiveNames: string[],
  modHashes: ModHashResult,
): {
  lines: { type: LogEntry["type"]; message: string }[];
  labels: Map<number, string>;
} {
  const out: { type: LogEntry["type"]; message: string }[] = [];
  const labels = new Map<number, string>();

  const modArchives = new Map<number, Set<string>>();
  for (const asset of assets) {
    if (!isModArchive(archiveNames[asset.archive_index])) continue;
    let ids = modArchives.get(asset.archive_index);
    if (!ids) {
      ids = new Set();
      modArchives.set(asset.archive_index, ids);
    }
    ids.add(asset.id);
  }

  if (modArchives.size === 0) {
    out.push({ type: "info", message: "No d\\mods\\* archives in this TOC — nothing to attribute." });
    return { lines: out, labels };
  }

  // Per .stage package: how many of its ids landed in each mod archive.
  const perMod = new Map<string, Map<number, number>>();
  const totals = new Map<string, number>();
  for (const entry of modHashes.entries) {
    totals.set(entry.mod_file, (totals.get(entry.mod_file) ?? 0) + 1);
    for (const [archiveIndex, ids] of modArchives) {
      if (!ids.has(entry.asset_id)) continue;
      let counts = perMod.get(entry.mod_file);
      if (!counts) {
        counts = new Map();
        perMod.set(entry.mod_file, counts);
      }
      counts.set(archiveIndex, (counts.get(archiveIndex) ?? 0) + 1);
    }
  }

  // Best claimant per archive, so two packages sharing assets don't both win.
  const claimed = new Map<number, { modFile: string; matched: number }>();
  for (const [modFile, counts] of perMod) {
    for (const [archiveIndex, matched] of counts) {
      const current = claimed.get(archiveIndex);
      if (!current || matched > current.matched) {
        claimed.set(archiveIndex, { modFile, matched });
      }
    }
  }

  for (const [archiveIndex, ids] of [...modArchives].sort((a, b) => a[0] - b[0])) {
    const name = archiveNames[archiveIndex] ?? `archive_${archiveIndex}`;
    const hit = claimed.get(archiveIndex);
    const slot = name.split(/[\\/]/).pop() ?? name;
    if (!hit) {
      labels.set(archiveIndex, slot);
      out.push({
        type: "warning",
        message: `${name}: no .stage in the library accounts for it — its ${ids.size} assets stay [UNKNOWN].`,
      });
      continue;
    }
    labels.set(archiveIndex, `${slot} — ${hit.modFile.replace(/\.stage$/i, "")}`);
    const total = totals.get(hit.modFile) ?? 0;
    const claimedSlot = modHashes.installed.indexOf(hit.modFile);
    const profileSlot = claimedSlot >= 0 ? `d\\mods\\mod${claimedSlot}` : null;
    const disagrees = profileSlot !== null && profileSlot.toLowerCase() !== name.replace(/\//g, "\\").toLowerCase();
    out.push({
      type: disagrees ? "warning" : "success",
      message: disagrees
        ? `${name} ← ${hit.modFile} (${hit.matched}/${total} ids present) — profile claims ${profileSlot}; trusting the TOC.`
        : `${name} ← ${hit.modFile} (${hit.matched}/${total} ids present)`,
    });
  }

  const matchedFiles = new Set([...claimed.values()].map((c) => c.modFile));
  for (const modFile of modHashes.installed) {
    if (matchedFiles.has(modFile)) continue;
    out.push({
      type: "warning",
      message: `Profile lists ${modFile} as installed, but none of its assets are in the TOC — install is stale or was reverted.`,
    });
  }

  return { lines: out, labels };
}

const MODS_ROOT = "[MODS]";

function buildTree(
  assets: AssetInfo[],
  hashMap: Map<string, string>,
  archiveNames: string[],
  modLabels?: Map<number, string>,
): TreeNodeData {
  const root: TreeNodeData = { name: "", fullPath: "", children: new Map() };

  function ensurePath(parts: string[], pinnedRoot = false): TreeNodeData {
    let current = root;
    let fullPath = "";
    for (const part of parts) {
      fullPath = fullPath ? `${fullPath}/${part}` : part;
      let child = current.children.get(part);
      if (!child) {
        child = { name: part, fullPath, children: new Map() };
        if (pinnedRoot && current === root) child.pinned = true;
        current.children.set(part, child);
      }
      current = child;
    }
    return current;
  }

  function attachSpan(node: TreeNodeData, asset: AssetInfo) {
    const spanEntry = { span: asset.span, size: asset.size, archiveIndex: asset.archive_index };
    if (node.asset) {
      // Same asset ID appearing in a different span (e.g. SD→HD texture pair) — merge.
      node.asset.spans.push(spanEntry);
      node.asset.spans.sort((a, b) => a.span - b.span);
    } else {
      node.asset = { id: asset.id, spans: [spanEntry] };
    }
  }

  for (const asset of assets) {
    const resolvedPath = hashMap.get(asset.id);
    const archiveName = archiveNames[asset.archive_index] ?? `archive_${asset.archive_index}`;
    const parts = resolvedPath
      ? resolvedPath.split("/").filter(Boolean)
      : ["[UNKNOWN]", archiveName, asset.id];

    const node = ensurePath(parts);
    attachSpan(node, asset);

    // Everything shipped by a mod also gets a pinned copy grouped by archive,
    // so mod content is browsable without hunting through the game tree.
    if (isModArchive(archiveName)) {
      const label =
        modLabels?.get(asset.archive_index) ?? archiveName.split(/[\\/]/).pop() ?? archiveName;
      const modNode = ensurePath(
        [MODS_ROOT, label, ...(resolvedPath ? parts : [asset.id])],
        true,
      );
      modNode.canonicalPath = node.fullPath;
      attachSpan(modNode, asset);
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
    const sorted = Array.from(node.children.values()).sort(compareNodes);
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

/// Mirror of the backend `is_mod_archive` helper — case-insensitive check
/// for the `d\mods\*` (or `d/mods/*`) override prefix used at runtime.
function isModArchive(name: string | undefined): boolean {
  if (!name) return false;
  return name.replace(/\//g, "\\").toLowerCase().startsWith("d\\mods\\");
}

type SourceMode = "live" | "require_toc_bak";

const SOURCE_MODE_LABELS: Record<SourceMode, string> = {
  live: "Live TOC (default)",
  require_toc_bak: "Require toc.BAK (clean, fail if missing)",
};

export default function AssetBrowser() {
  const { settings } = useSettings();
  const archivesDir = settings.archivesDir;
  const overstrikeDir = settings.overstrikeDir;
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

  const { projects, selectedProject, setSelectedProject, createProject, refreshProjects } = useProjects();
  const [extracting, setExtracting] = useState(false);
  const [sourceMode, setSourceMode] = useState<SourceMode>("live");

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
  const [refsModal, setRefsModal] = useState<{ assetId: string; assetPath: string } | null>(null);
  const [revealPath, setRevealPath] = useState<string | null>(null);
  const hashMapRef = useRef<Map<string, string>>(new Map());
  const byIdRef = useRef<Map<string, string>>(new Map()); // asset_id -> tree fullPath

  useEffect(() => {
    const dismiss = () => setCtxMenu(null);
    window.addEventListener("click", dismiss);
    return () => window.removeEventListener("click", dismiss);
  }, []);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

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

      // Mod-added assets are absent from the shipped list; their real paths
      // live in the .stage packages Overstrike installed them from.
      let modLabels: Map<number, string> | undefined;
      if (overstrikeDir) {
        try {
          const modHashes: ModHashResult = await invoke("load_mod_hashes", {
            overstrikeDir,
            gameDir: archivesDir,
          });
          for (const entry of modHashes.entries) {
            hashMap.set(entry.asset_id, entry.path);
          }
          setHashCount(hashMap.size);
          pushLog(
            "success",
            `Mod names: ${modHashes.entries.length} paths from ${modHashes.mods_read} .stage package(s)`
          );
          for (const note of modHashes.notes) pushLog("info", note);
          const verified = verifyModArchives(assets, info.archive_names, modHashes);
          modLabels = verified.labels;
          for (const line of verified.lines) {
            pushLog(line.type, line.message);
          }
        } catch (e) {
          pushLog("warning", `Could not read Overstrike mod names. (${e})`);
        }
      }

      // Automatically resolve streamed WEM assets from unhashed Wwise IDs
      for (const asset of assets) {
        if (!hashMap.has(asset.id)) {
          const archiveName = info.archive_names[asset.archive_index] ?? `archive_${asset.archive_index}`;
          const wwisePath = resolveWwisePath(asset.id, archiveName);
          if (wwisePath) {
            hashMap.set(asset.id, wwisePath);
          }
        }
      }

      hashMapRef.current = hashMap;

      const treeRoot = buildTree(assets, hashMap, info.archive_names, modLabels);
      const { assetMap, flatLeafOrder } = buildAssetIndex(treeRoot);
      assetMapRef.current = assetMap;
      flatLeafOrderRef.current = flatLeafOrder;

      const byId = new Map<string, string>();
      for (const [path, node] of assetMap) {
        if (node.asset) byId.set(node.asset.id.toUpperCase(), path);
      }
      byIdRef.current = byId;

      setTree(treeRoot);
      pushLog("success", `Tree built with ${assets.length} assets`);

      await refreshProjects();
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setLoading(false);
    }
  }, [archivesDir, overstrikeDir, refreshProjects]);

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
            assetPath: assetPathOf(node),
            sourceMode,
          });
          pushLog("success", `→ ${assetPathOf(node)}: ${result}`);
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
      await createProject(newProjName.trim(), newProjAuthor.trim());
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
      pushLog("info", `Extracting ${assetPathOf(node)} to temp…`);
      const tempPath: string = await invoke("extract_to_temp", {
        tocPath: tocPathRef.current,
        assetId: node.asset.id,
        archivesDir,
        filename: assetPathOf(node),
        sourceMode,
      });
      openToolWindow(route, { filePath: tempPath, assetPath: assetPathOf(node) }, settings.launchToolsInNewWindows);
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
        assetPath: assetPathOf(node),
        sourceMode,
      });
      pushLog("success", `Extracted ${assetPathOf(node)} → ${result}`);
    } catch (e) {
      pushLog("error", `Extract to path failed: ${e}`);
    }
  }

  function openReferencesFor(node: TreeNodeData) {
    setCtxMenu(null);
    if (!node.asset) return;
    if (!tocPathRef.current || !archivesDir) {
      pushLog("error", "Load a TOC first.");
      return;
    }
    setRefsModal({ assetId: node.asset.id, assetPath: assetPathOf(node) });
  }

  const handleJumpToAsset = useCallback((assetId: string, _resolvedPath: string | null) => {
    const path = byIdRef.current.get(assetId.toUpperCase());
    if (!path) {
      pushLog("warning", `Asset ${assetId} is not in the loaded TOC.`);
      return;
    }
    setSelectedPaths(new Set([path]));
    lastClickedPathRef.current = path;
    setRefsModal(null);
    // Force a state change even if the same path is jumped-to twice in a
    // row, so TreeNode `useEffect`s re-fire and scroll/expand again.
    setRevealPath(null);
    requestAnimationFrame(() => setRevealPath(path));
    // Clear the marker after the scroll animation so manual collapses
    // aren't immediately undone.
    window.setTimeout(() => setRevealPath(null), 1500);
  }, []);

  async function handleCopyAssetPath(node: TreeNodeData) {
    setCtxMenu(null);
    try {
      await navigator.clipboard.writeText(assetPathOf(node));
      pushLog("success", `Copied asset path: ${assetPathOf(node)}`);
    } catch (e) {
      pushLog("error", `Copy path failed: ${e}`);
    }
  }

  async function handleExportAsDds(node: TreeNodeData) {
    setCtxMenu(null);
    if (!node.asset || !tocPathRef.current || !archivesDir) return;

    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Export Texture as DDS (Select Output Folder)",
      });

      if (!selected || Array.isArray(selected)) return;

      pushLog("info", `Exporting ${assetPathOf(node)} to DDS…`);
      const result: string = await invoke("extract_asset_as_dds", {
        tocPath: tocPathRef.current,
        assetId: node.asset.id,
        archivesDir,
        outputDir: selected,
        assetPath: assetPathOf(node),
        sourceMode,
      });

      pushLog("success", `Exported to DDS: ${result}`);
    } catch (e) {
      pushLog("error", `Export as DDS failed: ${e}`);
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
  // How many selected assets have at least one span sourced from `d\mods\*`.
  const modSourceCount = tocInfo
    ? [...selectedPaths].reduce((count, p) => {
      const n = assetMapRef.current.get(p);
      const hit = n?.asset?.spans.some((s) =>
        isModArchive(tocInfo.archive_names[s.archiveIndex])
      );
      return count + (hit ? 1 : 0);
    }, 0)
    : 0;

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <div className={styles.headerTitle}>
          <h2 className={styles.title}>Asset Browser</h2>
          <span className={styles.subtitle}>Browse and extract game assets</span>
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
                  revealPath={revealPath}
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
                      <span className={styles.detailValuePath}>{assetPathOf(singleNode)}</span>
                    </div>
                    <div className={styles.detailGroup}>
                      <label>Asset ID</label>
                      <span className={styles.detailValueMono}>{singleNode.asset.id}</span>
                    </div>
                    <div className={styles.detailGroup}>
                      <label>Spans</label>
                      <div className={styles.spansList}>
                        {singleNode.asset.spans.map((s, idx) => {
                          const isTexture = assetPathOf(singleNode).toLowerCase().endsWith(".texture");
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
                    {tocInfo && (
                      <div className={styles.detailGroup}>
                        <label>Extraction Source</label>
                        <div className={styles.spansList}>
                          {singleNode.asset.spans.map((s, idx) => {
                            const archiveName =
                              tocInfo.archive_names[s.archiveIndex] ?? `archive_${s.archiveIndex}`;
                            const fromMod = isModArchive(archiveName);
                            const isTexture = assetPathOf(singleNode).toLowerCase().endsWith(".texture");
                            const spanLabel = isTexture
                              ? (s.span === 0 ? "SD" : s.span === 1 ? "HD" : `S${s.span}`)
                              : `S${s.span}`;
                            return (
                              <div
                                key={idx}
                                className={`${styles.spanItem} ${fromMod ? styles.sourceWarning : ""}`}
                                title={fromMod ? "This span resolves to a mod override archive — extracted bytes may differ from clean game data." : archiveName}
                              >
                                <span className={styles.spanTag}>{spanLabel}</span>
                                <span className={styles.sourcePath}>{archiveName}</span>
                                {fromMod && <span className={styles.modBadge}>MOD</span>}
                              </div>
                            );
                          })}
                        </div>
                        {singleNode.asset.spans.some((s) =>
                          isModArchive(tocInfo.archive_names[s.archiveIndex])
                        ) && (
                          <div className={styles.sourceWarningNote}>
                            Mod override — extract using <code>Require toc.BAK</code> mode for clean assets.
                          </div>
                        )}
                      </div>
                    )}
                    <div className={styles.detailGroup}>
                      <button
                        className={styles.newProjBtnSmall}
                        style={{ alignSelf: "flex-start", padding: "0.35rem 0.75rem" }}
                        onClick={() => openReferencesFor(singleNode)}
                        title="Discover assets referenced by, or referencing, this asset"
                      >
                        View References
                      </button>
                    </div>
                    {assetPathOf(singleNode).toLowerCase().endsWith(".texture") && tocPathRef.current && archivesDir && (
                      <TocTextureViewer
                        assetPath={assetPathOf(singleNode)}
                        tocPath={tocPathRef.current}
                        assetId={singleNode.asset.id}
                        archivesDir={archivesDir}
                      />
                    )}
                    {assetPathOf(singleNode).toLowerCase().endsWith(".soundbank") && tocPathRef.current && archivesDir && (
                      <TocSoundbankViewer
                        assetPath={assetPathOf(singleNode)}
                        tocPath={tocPathRef.current}
                        assetId={singleNode.asset.id}
                        archivesDir={archivesDir}
                      />
                    )}
                    {assetPathOf(singleNode).toLowerCase().endsWith(".wem") && tocPathRef.current && archivesDir && (
                      <TocWemViewer
                        assetPath={assetPathOf(singleNode)}
                        tocPath={tocPathRef.current}
                        assetId={singleNode.asset.id}
                        archivesDir={archivesDir}
                        sourceMode={sourceMode}
                      />
                    )}
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
                    {tocInfo && modSourceCount > 0 && (
                      <div className={styles.sourceWarningNote}>
                        {modSourceCount}/{selectionCount} from <code>d\mods\*</code>.
                      </div>
                    )}
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
                      <div className={styles.detailGroup}>
                        <label>Source mode</label>
                        <select
                          className={styles.projectSelectFull}
                          value={sourceMode}
                          onChange={(e) => setSourceMode(e.target.value as SourceMode)}
                          title="Controls which TOC the extraction commands read. Affects context-menu Extract / Send To / Stager (extract) too."
                        >
                          {(Object.keys(SOURCE_MODE_LABELS) as SourceMode[]).map((m) => (
                            <option key={m} value={m}>{SOURCE_MODE_LABELS[m]}</option>
                          ))}
                        </select>
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
        const ext = extOf(assetPathOf(ctxMenu.node));
        const targets = SEND_TO_ROUTES[ext] ?? [];
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
              {ext === "texture" && (
                <div className={styles.ctxItem} onClick={() => handleExportAsDds(ctxMenu.node)}>
                  Export as DDS
                </div>
              )}
              <div className={styles.ctxItem} onClick={() => handleCopyAssetPath(ctxMenu.node)}>
                Copy Asset Path
              </div>
              <div className={styles.ctxItem} onClick={() => openReferencesFor(ctxMenu.node)}>
                View References
              </div>
              {targets.length > 0 && (
                <>
                  <div style={{ height: 1, background: "var(--border)", margin: "4px 0" }} />
                  <div style={{ padding: "3px 16px 2px", fontSize: "0.68rem", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.07em", color: "var(--text-muted)" }}>
                    Send To
                  </div>
                  {targets.map((t) => (
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
                          filename: assetPathOf(ctxMenu.node),
                          sourceMode,
                        });
                        setSendToStager({ file: tempPath, defaultPath: `0/${assetPathOf(ctxMenu.node)}` });
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

      {refsModal && tocPathRef.current && archivesDir && (
        <AssetReferencesModal
          tocPath={tocPathRef.current}
          archivesDir={archivesDir}
          assetId={refsModal.assetId}
          assetPath={refsModal.assetPath}
          sourceMode={sourceMode}
          hashMap={hashMapRef.current}
          onClose={() => setRefsModal(null)}
          onJumpToAsset={handleJumpToAsset}
          onLog={pushLog}
        />
      )}

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
