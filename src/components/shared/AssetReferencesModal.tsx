import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useProjects } from "../../contexts/ProjectsContext";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { FaArrowRight } from "react-icons/fa";
import styles from "./AssetReferencesModal.module.css";

export interface AssetReferenceItem {
  depth: number;
  asset_id: string;
  filename: string | null;
  referenced_in: string[];
  in_toc: boolean;
  archive_name: string | null;
  size: number | null;
}

interface ReferenceResult {
  asset_id: string;
  direction: string;
  depth: number;
  references: AssetReferenceItem[];
  total_found: number;
  scanned: number;
  elapsed_ms: number;
  notes: string[];
  cancelled: boolean;
}

interface ScanProgress {
  scan_id: string;
  scanned: number;
  total: number;
  elapsed_ms: number;
  mem_bytes: number;
  cpu_percent: number;
}

function formatBytes(b: number): string {
  if (!b) return "—";
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(0)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

type Direction = "to" | "from";

function newScanId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `scan-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function formatEta(scanned: number, total: number, elapsedMs: number): string {
  if (scanned <= 0 || total <= 0) return "—";
  const remaining = Math.max(0, total - scanned);
  const ms = (elapsedMs / scanned) * remaining;
  if (!isFinite(ms) || ms <= 0) return "—";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${s % 60}s`;
}

interface Props {
  tocPath: string;
  archivesDir: string;
  assetId: string;
  assetPath: string;
  sourceMode: string;
  hashMap?: Map<string, string>;
  onClose: () => void;
  onJumpToAsset?: (assetId: string, resolvedPath: string | null) => void;
  onLog?: (level: "info" | "success" | "warning" | "error", message: string) => void;
}

function basename(path: string): string {
  const parts = path.replace(/\\+/g, "/").split("/").filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

/** Decode a backend `<hex>::<label>` source tag into a display string.
 *  - `Strings Block` tags collapse into the source asset's basename
 *    (since the section type carries no extra info).
 *  - All other tags drop the hex prefix and keep the section label
 *    as-is. The hex id is exposed via the chip's `title` for hover. */
function resolveSource(
  raw: string,
  hashMap: Map<string, string> | undefined,
  originHex: string,
  originPath: string,
): { display: string; tooltip: string } {
  const sep = raw.indexOf("::");
  const hex = sep >= 0 ? raw.slice(0, sep).toUpperCase() : "";
  const label = sep >= 0 ? raw.slice(sep + 2) : raw;

  let path: string | undefined;
  if (hex === originHex.toUpperCase()) path = originPath;
  if (!path) path = hashMap?.get(hex);

  const sourceName = path ? basename(path) : hex ? `[${hex.slice(0, 8)}…]` : "";
  const tooltip = path ?? hex;

  if (label === "Strings Block") {
    return { display: sourceName || label, tooltip: `${tooltip} · Strings Block` };
  }
  // For structured ref-section tags, keep the section label visible but
  // also annotate with the source asset on hover.
  return { display: label, tooltip: tooltip ? `${tooltip} · ${label}` : label };
}

function sanitizeAssetPath(filename: string | null, assetId: string): string {
  if (!filename || !filename.trim()) {
    return `unknown/${assetId}.bin`;
  }
  // Normalize separators and strip leading slashes; keep extension as-is.
  return filename.replace(/\\+/g, "/").replace(/^\/+/, "");
}

function formatSize(bytes: number | null | undefined): string {
  if (bytes == null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

export default function AssetReferencesModal({
  tocPath,
  archivesDir,
  assetId,
  assetPath,
  sourceMode,
  hashMap,
  onClose,
  onJumpToAsset,
  onLog,
}: Props) {
  const [direction, setDirection] = useState<Direction>("to");
  const [depth, setDepth] = useState(1);
  const [filter, setFilter] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<ReferenceResult | null>(null);
  const [error, setError] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const { projects, selectedProject, setSelectedProject } = useProjects();
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const activeScanIdRef = useRef<string | null>(null);
  const [limitThreads, setLimitThreads] = useState(true);
  const [typeFilterEnabled, setTypeFilterEnabled] = useState(true);
  // Sensible default of extensions known to embed inbound references.
  // Editable so power users can broaden / narrow the scan.
  const [typeFilterText, setTypeFilterText] = useState(
    ".config,.conduit,.actor,.zone,.nodegraph,.cinematic2,.material,.materialgraph,.model,.atmosphere",
  );

  // Listen once for backend-emitted progress events. Match by scan id so
  // stale events from prior scans don't update the bar.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    listen<ScanProgress>("references://progress", (evt) => {
      const p = evt.payload;
      if (activeScanIdRef.current && p.scan_id === activeScanIdRef.current) {
        setProgress(p);
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // Cleaned up local project loading, handled by useProjects context

  // Auto-run outbound scan on first open.
  useEffect(() => {
    if (!result && direction === "to") {
      void runScan(direction, depth);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const runScan = useCallback(
    async (dir: Direction, d: number) => {
      const scanId = newScanId();
      activeScanIdRef.current = scanId;
      setLoading(true);
      setError("");
      setProgress(null);

      // Build the optional asset-id allowlist for inbound scans. Only
      // applied when the user has the "filter by type" toggle on AND we
      // have a hash map to derive filenames from.
      let assetIdAllowlist: string[] | undefined;
      if (dir === "from" && typeFilterEnabled && hashMap) {
        const exts = typeFilterText
          .split(",")
          .map((s) => s.trim().toLowerCase())
          .filter((s) => s.length > 0)
          .map((s) => (s.startsWith(".") ? s : `.${s}`));
        if (exts.length > 0) {
          const ids: string[] = [];
          hashMap.forEach((path, hex) => {
            const lower = path.toLowerCase();
            if (exts.some((e) => lower.endsWith(e))) ids.push(hex);
          });
          assetIdAllowlist = ids;
        }
      }

      try {
        const r = await invoke<ReferenceResult>("get_asset_references", {
          tocPath,
          assetId,
          archivesDir,
          direction: dir,
          depth: d,
          sourceMode,
          scanId,
          assetIdAllowlist,
          limitThreads: dir === "from" ? limitThreads : undefined,
        });
        setResult(r);
      } catch (e) {
        setError(String(e));
      } finally {
        if (activeScanIdRef.current === scanId) {
          activeScanIdRef.current = null;
        }
        setLoading(false);
        setProgress(null);
      }
    },
    [
      tocPath,
      assetId,
      archivesDir,
      sourceMode,
      hashMap,
      typeFilterEnabled,
      typeFilterText,
      limitThreads,
    ],
  );

  const cancelScan = useCallback(() => {
    const id = activeScanIdRef.current;
    if (!id) return;
    invoke("cancel_asset_references", { scanId: id }).catch(() => {});
  }, []);

  const handleClose = useCallback(() => {
    if (activeScanIdRef.current) {
      const id = activeScanIdRef.current;
      invoke("cancel_asset_references", { scanId: id }).catch(() => {});
    }
    onClose();
  }, [onClose]);

  // Augment references with resolved filenames from the hash map when the
  // backend couldn't recover one from the strings pool.
  const augmented = useMemo<AssetReferenceItem[]>(() => {
    if (!result) return [];
    return result.references.map((r) => {
      if (!r.filename && hashMap) {
        const resolved = hashMap.get(r.asset_id);
        if (resolved) return { ...r, filename: resolved };
      }
      return r;
    });
  }, [result, hashMap]);

  const filtered = useMemo(() => {
    if (!filter.trim()) return augmented;
    const q = filter.toLowerCase();
    return augmented.filter(
      (r) =>
        r.asset_id.toLowerCase().includes(q) ||
        (r.filename?.toLowerCase().includes(q) ?? false) ||
        r.referenced_in.some((s) => s.toLowerCase().includes(q)) ||
        (r.archive_name?.toLowerCase().includes(q) ?? false),
    );
  }, [augmented, filter]);

  function handleBackdrop(e: React.MouseEvent) {
    if (e.target === e.currentTarget) handleClose();
  }

  // Reset selection when scan results change.
  useEffect(() => {
    setSelected(new Set());
  }, [result]);

  function toggleRow(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  const visibleExtractable = useMemo(
    () => filtered.filter((r) => r.in_toc),
    [filtered],
  );

  const allVisibleSelected =
    visibleExtractable.length > 0 &&
    visibleExtractable.every((r) => selected.has(r.asset_id));

  function toggleAllVisible() {
    setSelected((prev) => {
      const next = new Set(prev);
      if (allVisibleSelected) {
        for (const r of visibleExtractable) next.delete(r.asset_id);
      } else {
        for (const r of visibleExtractable) next.add(r.asset_id);
      }
      return next;
    });
  }

  const selectedItems = useMemo(
    () => augmented.filter((r) => selected.has(r.asset_id) && r.in_toc),
    [augmented, selected],
  );

  async function handleExport() {
    if (!result) return;
    const header = "depth,asset_id,filename,in_toc,archive,size,sources";
    const rows = filtered.map((r) => {
      const sources = r.referenced_in
        .map((s) => resolveSource(s, hashMap, assetId, assetPath).display)
        .join("; ")
        .replace(/"/g, "\"\"");
      const fn = (r.filename ?? "").replace(/"/g, "\"\"");
      const arc = (r.archive_name ?? "").replace(/"/g, "\"\"");
      return `${r.depth},${r.asset_id},"${fn}",${r.in_toc},"${arc}",${r.size ?? ""},"${sources}"`;
    });
    const csv = [header, ...rows].join("\n");
    try {
      const path = await saveDialog({
        title: "Export references as CSV",
        defaultPath: `references_${assetId}_${direction}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (!path) return;
      await invoke("write_text_file", { path, contents: csv });
      onLog?.("success", `Exported ${rows.length} references → ${path}`);
    } catch (e) {
      const msg = `CSV export failed: ${e}`;
      setError(msg);
      onLog?.("error", msg);
    }
  }

  async function extractMany(
    items: AssetReferenceItem[],
    runOne: (item: AssetReferenceItem) => Promise<string>,
    label: string,
  ) {
    if (items.length === 0) return;
    setBusy(label);
    setError("");
    let ok = 0;
    let failed = 0;
    for (const item of items) {
      try {
        await runOne(item);
        ok += 1;
      } catch (e) {
        failed += 1;
        onLog?.("error", `Extract ${item.asset_id} failed: ${e}`);
      }
    }
    setBusy(null);
    const summary = `${label}: ${ok} ok${failed ? `, ${failed} failed` : ""}`;
    onLog?.(failed ? "warning" : "success", summary);
    if (failed && !ok) setError(summary);
  }

  async function handleExtractToProject() {
    if (!selectedProject) {
      setError("Pick a project first (or create one in the Stager).");
      return;
    }
    await extractMany(
      selectedItems,
      async (item) => {
        const rel = sanitizeAssetPath(item.filename, item.asset_id);
        return await invoke<string>("extract_asset_to_project", {
          tocPath,
          assetId: item.asset_id,
          archivesDir,
          projectName: selectedProject,
          assetPath: rel,
          sourceMode,
        });
      },
      `Extracted to project '${selectedProject}'`,
    );
  }

  async function handleExtractToFolder() {
    const dir = await openDialog({
      title: "Pick output folder",
      directory: true,
      multiple: false,
    });
    if (!dir || typeof dir !== "string") return;
    await extractMany(
      selectedItems,
      async (item) => {
        const rel = sanitizeAssetPath(item.filename, item.asset_id);
        return await invoke<string>("extract_asset_to_path", {
          tocPath,
          assetId: item.asset_id,
          archivesDir,
          outputDir: dir,
          assetPath: rel,
          sourceMode,
        });
      },
      `Extracted to ${dir}`,
    );
  }

  return (
    <div className={styles.backdrop} onClick={handleBackdrop}>
      <div className={styles.modal}>
        <div className={styles.titleRow}>
          <h3 className={styles.title}>Asset References</h3>
          <span className={styles.subtitle}>
            {assetPath} <span style={{ opacity: 0.6 }}>· {assetId}</span>
          </span>
        </div>

        <div className={styles.controls}>
          <div className={styles.field}>
            <label>Direction</label>
            <select
              className={styles.select}
              value={direction}
              onChange={(e) => setDirection(e.target.value as Direction)}
            >
              <option value="to">References To (outbound)</option>
              <option value="from">References From (inbound, slow)</option>
            </select>
          </div>
          <div className={styles.field}>
            <label>Depth</label>
            <select
              className={styles.select}
              value={depth}
              onChange={(e) => setDepth(parseInt(e.target.value, 10))}
              disabled={direction === "from"}
              title={direction === "from" ? "Depth is fixed to 1 for inbound search" : ""}
            >
              {[1, 2, 3, 4, 5].map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
          </div>
          <div className={`${styles.field} ${styles.filter}`}>
            <label>Filter</label>
            <input
              className={styles.input}
              placeholder="Filter by path, id, source, archive…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
          <button
            className={styles.scanBtn}
            onClick={() => runScan(direction, depth)}
            disabled={loading}
          >
            {loading ? "Scanning…" : "Scan"}
          </button>
          {loading && (
            <button
              className={styles.bulkBtnGhost}
              onClick={cancelScan}
              title="Cancel the in-flight scan"
            >
              Cancel scan
            </button>
          )}
        </div>

        {direction === "from" && (
          <div
            className={styles.bulkBar}
            style={{ alignItems: "center", gap: "0.75rem" }}
          >
            <label
              style={{
                display: "flex",
                alignItems: "center",
                gap: "0.35rem",
                fontSize: "0.78rem",
              }}
              title="Run the scan on ~50% of CPU cores at BELOW_NORMAL priority so the rest of the system stays responsive"
            >
              <input
                type="checkbox"
                checked={limitThreads}
                onChange={(e) => setLimitThreads(e.target.checked)}
                disabled={loading}
              />
              Limit CPU usage (safe mode)
            </label>
            <label
              style={{
                display: "flex",
                alignItems: "center",
                gap: "0.35rem",
                fontSize: "0.78rem",
              }}
              title="Restrict the scan to assets whose resolved filename ends with one of the listed extensions"
            >
              <input
                type="checkbox"
                checked={typeFilterEnabled}
                onChange={(e) => setTypeFilterEnabled(e.target.checked)}
                disabled={loading}
              />
              Scan only ref-bearing types
            </label>
            <input
              className={styles.input}
              style={{ flex: 1, minWidth: 240, fontSize: "0.75rem" }}
              value={typeFilterText}
              onChange={(e) => setTypeFilterText(e.target.value)}
              disabled={loading || !typeFilterEnabled}
              placeholder=".config,.actor,.zone,…"
              title="Comma-separated list of file extensions to include in the scan"
            />
          </div>
        )}

        {direction === "from" && !loading && !result && (
          <div className={styles.warn}>
            Inbound scan extracts every span-0 asset in the TOC (in parallel).
            On a full game TOC this typically takes 30–120 seconds.
          </div>
        )}

        {loading && direction === "from" && (
          <div className={styles.warn}>
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                marginBottom: "0.4rem",
                gap: "1rem",
                fontSize: "0.78rem",
              }}
            >
              <span>
                {progress
                  ? `Scanning ${progress.scanned.toLocaleString()} / ${progress.total.toLocaleString()} assets`
                  : "Preparing inbound scan…"}
              </span>
              <span style={{ fontFamily: "var(--font-mono)" }}>
                {progress
                  ? `${(progress.elapsed_ms / 1000).toFixed(1)}s · ETA ${formatEta(
                      progress.scanned,
                      progress.total,
                      progress.elapsed_ms,
                    )} · CPU ${progress.cpu_percent.toFixed(0)}% · RAM ${formatBytes(progress.mem_bytes)}`
                  : ""}
              </span>
            </div>
            <div
              style={{
                width: "100%",
                height: 6,
                background: "rgba(255,255,255,0.08)",
                borderRadius: 3,
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  width: progress && progress.total > 0
                    ? `${Math.min(100, (progress.scanned / progress.total) * 100).toFixed(1)}%`
                    : "0%",
                  height: "100%",
                  background: "var(--accent)",
                  transition: "width 0.2s linear",
                }}
              />
            </div>
          </div>
        )}

        {error && <div className={styles.error}>{error}</div>}

        {result && (
          <div className={styles.summary}>
            <span>
              Found <strong>{result.total_found}</strong> references
            </span>
            <span>
              Scanned <strong>{result.scanned}</strong> assets
            </span>
            <span>
              Elapsed <strong>{result.elapsed_ms} ms</strong>
            </span>
            {result.cancelled && (
              <span style={{ color: "#e0a050" }}>· cancelled</span>
            )}
            {filter && (
              <span>
                Visible <strong>{filtered.length}</strong>
              </span>
            )}
            <span>
              Selected <strong>{selectedItems.length}</strong>
            </span>
          </div>
        )}

        {result && (
          <div className={styles.bulkBar}>
            <button
              className={styles.bulkBtnGhost}
              onClick={toggleAllVisible}
              disabled={visibleExtractable.length === 0 || busy !== null}
              title="Select / deselect every in-TOC reference matching the current filter"
            >
              {allVisibleSelected ? "Deselect visible" : "Select visible"}
              {visibleExtractable.length > 0 && (
                <span className={styles.countHint}>({visibleExtractable.length})</span>
              )}
            </button>
            <button
              className={styles.bulkBtnGhost}
              onClick={() => setSelected(new Set())}
              disabled={selected.size === 0 || busy !== null}
            >
              Clear
            </button>
            <div className={styles.bulkSpacer} />
            <select
              className={styles.select}
              value={selectedProject}
              onChange={(e) => setSelectedProject(e.target.value)}
              disabled={projects.length === 0 || busy !== null}
              title={projects.length === 0 ? "No Stager projects yet" : "Target Stager project"}
            >
              {projects.length === 0 && <option value="">No projects</option>}
              {projects.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name}
                </option>
              ))}
            </select>
            <button
              className={styles.bulkBtn}
              onClick={handleExtractToProject}
              disabled={
                selectedItems.length === 0 ||
                !selectedProject ||
                busy !== null
              }
              title="Extract every selected reference into the chosen Stager project"
            >
              {busy && busy.startsWith("Extracted to project") ? "Extracting…" : `Extract → Project (${selectedItems.length})`}
            </button>
            <button
              className={styles.bulkBtnGhost}
              onClick={handleExtractToFolder}
              disabled={selectedItems.length === 0 || busy !== null}
              title="Extract every selected reference into a folder of your choice"
            >
              {busy && busy.startsWith("Extracted to ") && !busy.startsWith("Extracted to project") ? "Extracting…" : "Extract → Folder…"}
            </button>
          </div>
        )}

        <div className={styles.list}>
          <div className={`${styles.row} ${styles.header}`}>
            <span>
              <input
                type="checkbox"
                checked={allVisibleSelected}
                onChange={toggleAllVisible}
                disabled={visibleExtractable.length === 0}
                title="Select all visible (in-TOC)"
              />
            </span>
            <span>Depth</span>
            <span>Asset</span>
            <span>Referenced In</span>
            <span>Size</span>
            <span />
          </div>
          {!result && !loading && (
            <div className={styles.empty}>Click Scan to discover references.</div>
          )}
          {result && filtered.length === 0 && (
            <div className={styles.empty}>No references match the filter.</div>
          )}
          {filtered.map((r, idx) => {
            const path = r.filename ?? null;
            const isSelected = selected.has(r.asset_id);
            return (
              <div
                key={`${r.asset_id}-${idx}`}
                className={`${styles.row} ${onJumpToAsset ? styles.clickable : ""} ${isSelected ? styles.selectedRow : ""}`}
                onClick={() => onJumpToAsset?.(r.asset_id, path)}
                title={r.archive_name ?? ""}
              >
                <span onClick={(e) => e.stopPropagation()}>
                  <input
                    type="checkbox"
                    checked={isSelected}
                    disabled={!r.in_toc}
                    onChange={() => toggleRow(r.asset_id)}
                    title={r.in_toc ? "Select for bulk extract" : "Cannot extract — not in TOC"}
                  />
                </span>
                <span className={styles.depthBadge}>{r.depth}</span>
                <div className={styles.name}>
                  {path ? (
                    <span className={styles.namePath}>{path}</span>
                  ) : (
                    <span className={styles.nameUnresolved}>
                      [UNKNOWN] {r.asset_id}
                    </span>
                  )}
                  <span className={styles.nameMeta}>
                    <span style={{ fontFamily: "var(--font-mono)" }}>{r.asset_id}</span>
                    {r.archive_name && <> · {r.archive_name}</>}
                    {!r.in_toc && (
                      <> · <span className={styles.notInToc}>not in TOC</span></>
                    )}
                  </span>
                </div>
                <div className={styles.sources}>
                  {r.referenced_in.map((s, i) => {
                    const { display, tooltip } = resolveSource(
                      s,
                      hashMap,
                      assetId,
                      assetPath,
                    );
                    return (
                      <span key={i} className={styles.tag} title={tooltip}>
                        {display}
                      </span>
                    );
                  })}
                </div>
                <span>{formatSize(r.size)}</span>
                <button
                  className={styles.openIcon}
                  disabled={!onJumpToAsset || !r.in_toc}
                  title="Jump to asset in tree"
                  onClick={(e) => {
                    e.stopPropagation();
                    onJumpToAsset?.(r.asset_id, path);
                  }}
                >
                  <FaArrowRight />
                </button>
              </div>
            );
          })}
        </div>

        {result?.notes && result.notes.length > 0 && (
          <div className={styles.summary} style={{ flexWrap: "wrap" }}>
            {result.notes.map((n, i) => (
              <span key={i}>· {n}</span>
            ))}
          </div>
        )}

        <div className={styles.actions}>
          <button className={styles.cancelBtn} onClick={handleClose}>
            Close
          </button>
          <button
            className={styles.exportBtn}
            onClick={handleExport}
            disabled={!result || filtered.length === 0}
            title="Export current (filtered) results as CSV"
          >
            Export CSV
          </button>
        </div>
      </div>
    </div>
  );
}
