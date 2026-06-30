import { useState, useEffect, useCallback } from "react";
import { useSettings } from "../../contexts/SettingsContext";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import CustomAudioPlayer from "../../components/shared/CustomAudioPlayer";
import styles from "./BnkExplorer.module.css";

/* ── Types ────────────────────────────────────────────── */

interface WemEntry {
  id: number;
  offset: number;
  size: number;
  codec: string;
  sample_rate: number;
  channels: number;
  avg_bitrate: number;
}

interface HircEvent {
  id: number;
  name: string | null;
  wem_ids: number[];
}

interface BnkFullInfo {
  version: number;
  bank_id: number;
  language_id: number;
  bnk_size: number;
  wems: WemEntry[];
  events: HircEvent[];
}

interface BnkWemPreview {
  audio_src: string;
  codec: string;
  sample_rate: number;
  channels: number;
  avg_bitrate: number;
  size: number;
}

/* ── Helpers ──────────────────────────────────────────── */

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function toHex(n: number): string {
  return `0x${n.toString(16).toUpperCase()}`;
}

function formatDuration(seconds: number): string {
  if (isNaN(seconds) || !isFinite(seconds) || seconds < 0) return "0.00s";
  if (seconds < 60) {
    return `${seconds.toFixed(2)}s`;
  }
  const mins = Math.floor(seconds / 60);
  const secs = Math.floor(seconds % 60);
  return `${mins}:${secs.toString().padStart(2, "0")}`;
}

const BNK_FILTER = [{ name: "Wwise Bank", extensions: ["bnk", "soundbank"] }];

/* ── Component ────────────────────────────────────────── */

export default function BnkExplorer() {
  const { settings } = useSettings();
  const location = useLocation();
  const [filePath, setFilePath] = useState("");
  const [bankInfo, setBankInfo] = useState<BnkFullInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [selectedWemId, setSelectedWemId] = useState<number | null>(null);
  const [audioSrc, setAudioSrc] = useState<string | null>(null);
  const [previewInfo, setPreviewInfo] = useState<BnkWemPreview | null>(null);
  const [audioLoading, setAudioLoading] = useState(false);
  const [search, setSearch] = useState("");
  const [log, setLog] = useState<LogEntry[]>([]);
  const [expandedEvents, setExpandedEvents] = useState<Set<number>>(new Set());
  const [activeTab, setActiveTab] = useState<"events" | "wems">("events");
  const [wemSort, setWemSort] = useState<"id" | "length-desc" | "length-asc">("id");

  const [wemIdToPreviewNext, setWemIdToPreviewNext] = useState<number | null>(null);
  const [showBatchModal, setShowBatchModal] = useState(false);
  const [batchFolder, setBatchFolder] = useState("");
  const [scannedWems, setScannedWems] = useState<Record<number, string>>({});
  const [injecting, setInjecting] = useState(false);

  useEffect(() => {
    if (bankInfo && wemIdToPreviewNext !== null) {
      const id = wemIdToPreviewNext;
      setWemIdToPreviewNext(null);
      handleSelectWem(id);
    }
  }, [bankInfo, wemIdToPreviewNext]);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function handleReplaceWem(wemId: number) {
    try {
      const selectedFile = await open({
        multiple: false,
        filters: [{ name: "WEM Audio", extensions: ["wem"] }],
        title: "Select Replacement WEM File",
      });
      if (typeof selectedFile !== "string") return;

      pushLog("info", `Replacing WEM ${wemId} with '${selectedFile}'...`);
      setLoading(true);

      const replacements: Record<number, string> = { [wemId]: selectedFile };
      await invoke("bnk_batch_inject_wems", {
        path: filePath,
        replacements,
      });

      pushLog("success", `Successfully replaced WEM ${wemId}.`);
      setWemIdToPreviewNext(wemId);
      await loadBank(filePath);
    } catch (e) {
      pushLog("error", `Failed to replace WEM ${wemId}: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  async function handleSelectBatchFolder() {
    try {
      const dir = await open({ directory: true, title: "Select WEM Input Folder" });
      if (typeof dir !== "string") return;

      setBatchFolder(dir);
      pushLog("info", `Scanning folder: ${dir}`);
      const scanned = await invoke<Record<string, string>>("bnk_scan_wem_folder", {
        folderPath: dir,
      });

      const converted: Record<number, string> = {};
      Object.entries(scanned).forEach(([k, v]) => {
        converted[parseInt(k, 10)] = v;
      });
      setScannedWems(converted);
    } catch (e) {
      pushLog("error", `Failed to scan folder: ${e}`);
    }
  }

  async function handleExecuteBatchInjection(matches: { id: number; path: string }[]) {
    if (matches.length === 0) return;

    setInjecting(true);
    pushLog("info", `Injecting ${matches.length} WEM files into bank...`);

    try {
      const replacements: Record<number, string> = {};
      matches.forEach(m => {
        replacements[m.id] = m.path;
      });

      await invoke("bnk_batch_inject_wems", {
        path: filePath,
        replacements,
      });

      pushLog("success", `Successfully injected ${matches.length} WEMs.`);
      setShowBatchModal(false);

      if (selectedWemId && replacements[selectedWemId]) {
        setWemIdToPreviewNext(selectedWemId);
      }
      await loadBank(filePath);
    } catch (e) {
      pushLog("error", `Batch injection failed: ${e}`);
    } finally {
      setInjecting(false);
    }
  }

  /* ── Load bank ──────────────────────────────────────── */

  const loadBank = useCallback(async (path: string) => {
    if (!path) return;

    setLoading(true);
    setBankInfo(null);
    setSelectedWemId(null);
    setAudioSrc(null);
    setPreviewInfo(null);
    setExpandedEvents(new Set());
    setActiveTab("events");
    setWemSort("id");
    pushLog("info", `Parsing bank: ${path}`);

    try {
      const info = await invoke<BnkFullInfo>("bnk_parse_full", { path });
      setBankInfo(info);
      pushLog("success", `Loaded bank with ${info.wems.length} WEMs and ${info.events.length} events.`);
    } catch (e) {
      pushLog("error", `Failed to parse bank: ${e}`);
    } finally {
      setLoading(false);
    }
  }, []);

  async function handleLoadBank() {
    if (!filePath) {
      pushLog("error", "Please select a .bnk or .soundbank file first.");
      return;
    }
    await loadBank(filePath);
  }

  useEffect(() => {
    if (location.pathname !== "/tools/bnk-explorer") return;
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string } | null;
    const pathVal = s?.filePath ?? params.get("filePath") ?? undefined;
    if (pathVal) {
      setFilePath(pathVal);
      loadBank(pathVal);
    }
  }, [location.pathname, location.state, location.search, loadBank]);

  /* ── Audio preview ──────────────────────────────────── */

  const handlePreviewAudio = useCallback(
    async (wemId: number) => {
      setAudioLoading(true);
      setAudioSrc(null);
      setPreviewInfo(null);

      try {
        const preview = await invoke<BnkWemPreview>("wem_preview_audio", {
          bnkPath: filePath,
          wemId,
          archivesDir: settings.archivesDir || null,
        });
        setPreviewInfo(preview);
        setAudioSrc(preview.audio_src);
      } catch (e) {
        pushLog("error", `Failed to decode WEM ${wemId}: ${e}`);
        setAudioSrc(null);
        setPreviewInfo(null);
      } finally {
        setAudioLoading(false);
      }
    },
    [filePath, settings.archivesDir],
  );

  function handleSelectWem(wemId: number) {
    setSelectedWemId(wemId);
    setAudioSrc(null);
    handlePreviewAudio(wemId);
  }

  /* ── Extract ────────────────────────────────────────── */

  async function handleExtractWem(wemIds?: number[]) {
    const dir = await open({ directory: true, title: "Select Output Folder" });
    if (typeof dir !== "string") return;

    const label = wemIds ? `WEM ${wemIds.join(", ")}` : "all WEMs";
    pushLog("info", `Extracting ${label} to ${dir}...`);

    try {
      await invoke("bnk_extract_wems", {
        path: filePath,
        outputDir: dir,
        ...(wemIds ? { wemIds } : {}),
        archivesDir: settings.archivesDir || null,
      });
      pushLog("success", `Successfully extracted ${label}.`);
    } catch (e) {
      pushLog("error", `Extraction failed: ${e}`);
    }
  }

  /* ── Derived data ───────────────────────────────────── */

  const staticWem = bankInfo?.wems.find((w) => w.id === selectedWemId) ?? null;

  const displayWem = selectedWemId !== null ? {
    id: selectedWemId,
    codec: staticWem?.codec ?? previewInfo?.codec ?? "Unknown (Streamed)",
    sample_rate: staticWem?.sample_rate ?? previewInfo?.sample_rate ?? 0,
    channels: staticWem?.channels ?? previewInfo?.channels ?? 0,
    avg_bitrate: staticWem?.avg_bitrate ?? previewInfo?.avg_bitrate ?? 0,
    size: staticWem?.size ?? previewInfo?.size ?? 0,
    isStreamed: !staticWem,
  } : null;

  // Build set of WEM IDs referenced by events
  const referencedWemIds = new Set<number>();
  bankInfo?.events.forEach((ev) => ev.wem_ids.forEach((id) => referencedWemIds.add(id)));

  // Lookup WEM entry by ID for inline info
  const wemMap = new Map<number, WemEntry>();
  bankInfo?.wems.forEach((w) => wemMap.set(w.id, w));

  /* ── Search filter ──────────────────────────────────── */

  const lowerSearch = search.toLowerCase();

  const filteredEvents =
    bankInfo?.events.filter((ev) => {
      if (!search) return true;
      if (ev.name?.toLowerCase().includes(lowerSearch)) return true;
      if (ev.wem_ids.some((id) => id.toString().includes(lowerSearch))) return true;
      return false;
    }) ?? [];



  const filteredWems =
    bankInfo?.wems.filter((w) => {
      if (!search) return true;
      return (
        w.id.toString().includes(lowerSearch) ||
        w.codec.toLowerCase().includes(lowerSearch)
      );
    }) ?? [];

  const sortedWems = [...filteredWems].sort((a, b) => {
    if (wemSort === "length-desc") {
      const durA = a.avg_bitrate > 0 ? a.size / a.avg_bitrate : 0;
      const durB = b.avg_bitrate > 0 ? b.size / b.avg_bitrate : 0;
      return durB - durA || a.id - b.id;
    } else if (wemSort === "length-asc") {
      const durA = a.avg_bitrate > 0 ? a.size / a.avg_bitrate : 0;
      const durB = b.avg_bitrate > 0 ? b.size / b.avg_bitrate : 0;
      return durA - durB || a.id - b.id;
    } else {
      return a.id - b.id;
    }
  });

  /* ── Toggle helpers ─────────────────────────────────── */

  function toggleEvent(eventId: number) {
    setExpandedEvents((prev) => {
      const next = new Set(prev);
      if (next.has(eventId)) next.delete(eventId);
      else next.add(eventId);
      return next;
    });
  }

  /* ── Render helpers ─────────────────────────────────── */

  function renderWemRow(wemId: number) {
    const wem = wemMap.get(wemId);
    const isSelected = selectedWemId === wemId;
    const isOrphan = !referencedWemIds.has(wemId);

    let info = wem ? `${wem.codec}, ${wem.sample_rate}Hz, ${wem.channels}ch` : "";
    if (!wem && isSelected && previewInfo) {
      info = `${previewInfo.codec}, ${previewInfo.sample_rate}Hz, ${previewInfo.channels}ch`;
    }

    let duration = wem && wem.avg_bitrate > 0 ? wem.size / wem.avg_bitrate : 0;
    if (!wem && isSelected && previewInfo && previewInfo.avg_bitrate > 0) {
      duration = previewInfo.size / previewInfo.avg_bitrate;
    }
    const durationStr = duration > 0 ? formatDuration(duration) : "";

    return (
      <div key={wemId} className={styles.treeNode}>
        <div
          className={`${styles.treeRow} ${isSelected ? styles.selected : ""}`}
          onClick={() => handleSelectWem(wemId)}
        >
          <span className={styles.nodeIcon}>♪</span>
          <span className={styles.nodeName}>
            {wemId}.wem {info && <span style={{ color: "var(--text-muted)" }}>({info})</span>}
          </span>
          {durationStr && <span className={styles.wemDuration}>{durationStr}</span>}
          <span className={`${styles.nodeTag} ${isOrphan ? styles.nodeTagOrphan : styles.nodeTagWem}`}>
            {isOrphan ? "Orphan" : "WEM"}
          </span>
        </div>
      </div>
    );
  }

  function renderEventNode(ev: HircEvent) {
    const isExpanded = expandedEvents.has(ev.id);
    const name = ev.name ?? `Event_${ev.id}`;

    return (
      <div key={ev.id} className={styles.treeNode}>
        <div className={styles.treeRow} onClick={() => toggleEvent(ev.id)}>
          <span className={styles.nodeIcon}>{isExpanded ? "▾" : "▸"}</span>
          <span className={styles.nodeName}>{name}</span>
          <span className={`${styles.nodeTag} ${styles.nodeTagEvent}`}>Event</span>
        </div>
        {isExpanded && (
          <div className={styles.treeChildren}>
            {ev.wem_ids.length === 0 ? (
              <div className={styles.treeRow} style={{ color: "var(--text-muted)", cursor: "default" }}>
                <span className={styles.nodeIcon}>–</span>
                <span className={styles.nodeName}>No linked WEMs</span>
              </div>
            ) : (
              ev.wem_ids.map((id) => renderWemRow(id))
            )}
          </div>
        )}
      </div>
    );
  }

  /* ── JSX ────────────────────────────────────────────── */

  return (
    <div className={styles.page}>
      <h2 className={styles.title}>BNK Explorer</h2>
      <p className={styles.subtitle}>Inspect Wwise soundbank contents, preview audio, and extract WEMs</p>

      <div className={styles.mainLayout}>
        {/* ── Left Panel ─────────────────────────────── */}
        <div className={`${styles.column} ${styles.columnLeft}`}>
          <FilePickerInput
            label="Soundbank File"
            value={filePath}
            onChange={setFilePath}
            mode="open"
            filters={BNK_FILTER}
          />

          <button
            className={styles.loadBtn}
            onClick={handleLoadBank}
            disabled={loading || !filePath}
          >
            {loading ? "Parsing…" : "Load Bank"}
          </button>

          {bankInfo && (
            <button
              className={styles.secondaryBtn}
              style={{ marginTop: "0.5rem" }}
              onClick={() => {
                setBatchFolder("");
                setScannedWems({});
                setShowBatchModal(true);
              }}
              disabled={loading}
            >
              Batch Replace WEMs...
            </button>
          )}

          {bankInfo && (
            <>
              <h3 className={styles.sectionTitle}>Bank Header</h3>
              <div className={styles.bankInfoGrid}>
                <div className={styles.infoItem}>
                  <span className={styles.infoLabel}>Bank ID</span>
                  <span className={styles.infoValue}>{toHex(bankInfo.bank_id)}</span>
                </div>
                <div className={styles.infoItem}>
                  <span className={styles.infoLabel}>Version</span>
                  <span className={styles.infoValue}>{bankInfo.version}</span>
                </div>
                <div className={styles.infoItem}>
                  <span className={styles.infoLabel}>Project ID</span>
                  <span className={styles.infoValue}>{toHex(bankInfo.language_id)}</span>
                </div>
                <div className={styles.infoItem}>
                  <span className={styles.infoLabel}>File Size</span>
                  <span className={styles.infoValue}>{formatBytes(bankInfo.bnk_size)}</span>
                </div>
                <div className={styles.infoItem}>
                  <span className={styles.infoLabel}>WEMs</span>
                  <span className={styles.infoValue}>{bankInfo.wems.length}</span>
                </div>
                <div className={styles.infoItem}>
                  <span className={styles.infoLabel}>Events</span>
                  <span className={styles.infoValue}>{bankInfo.events.length}</span>
                </div>
              </div>
            </>
          )}
        </div>

        {/* ── Center Panel ───────────────────────────── */}
        <div className={`${styles.column} ${styles.columnCenter}`}>
          {bankInfo ? (
            <>
              <div className={styles.tabsHeader}>
                <button
                  className={`${styles.tabBtn} ${activeTab === "events" ? styles.activeTab : ""}`}
                  onClick={() => setActiveTab("events")}
                >
                  Events ({filteredEvents.length})
                </button>
                <button
                  className={`${styles.tabBtn} ${activeTab === "wems" ? styles.activeTab : ""}`}
                  onClick={() => setActiveTab("wems")}
                >
                  All WEMs ({filteredWems.length})
                </button>
              </div>

              <div className={styles.searchRow}>
                <input
                  className={styles.searchInput}
                  type="text"
                  placeholder={
                    activeTab === "events"
                      ? "Search events or linked WEM IDs…"
                      : "Search WEM IDs or codecs…"
                  }
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
                {activeTab === "wems" && (
                  <select
                    className={styles.sortSelect}
                    value={wemSort}
                    onChange={(e) => setWemSort(e.target.value as any)}
                    title="Sort WEM files"
                  >
                    <option value="id">Sort by ID</option>
                    <option value="length-desc">Length (Longest first)</option>
                    <option value="length-asc">Length (Shortest first)</option>
                  </select>
                )}
              </div>

              <div className={styles.treeContainer}>
                {activeTab === "events" ? (
                  <>
                    {filteredEvents.map((ev) => renderEventNode(ev))}
                    {filteredEvents.length === 0 && (
                      <div className={styles.emptyState}>No events found matching "{search}"</div>
                    )}
                  </>
                ) : (
                  <>
                    {sortedWems.map((w) => renderWemRow(w.id))}
                    {sortedWems.length === 0 && (
                      <div className={styles.emptyState}>No WEM files found matching "{search}"</div>
                    )}
                  </>
                )}
              </div>
            </>
          ) : (
            <div className={styles.emptyState}>
              Load a .bnk or .soundbank file to explore its contents
            </div>
          )}
        </div>

        {/* ── Right Panel ────────────────────────────── */}
        <div className={`${styles.column} ${styles.columnRight}`}>
          {displayWem ? (
            <>
              <h3 className={styles.sectionTitle}>WEM Details</h3>
              <div className={styles.wemDetails}>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>ID</span>
                  <span className={styles.detailValue}>{displayWem.id}</span>
                </div>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>Codec</span>
                  <span className={styles.detailValue}>{displayWem.codec}</span>
                </div>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>Sample Rate</span>
                  <span className={styles.detailValue}>
                    {displayWem.sample_rate > 0 ? `${displayWem.sample_rate} Hz` : "Unknown"}
                  </span>
                </div>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>Channels</span>
                  <span className={styles.detailValue}>
                    {displayWem.channels > 0 ? displayWem.channels : "Unknown"}
                  </span>
                </div>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>Bitrate</span>
                  <span className={styles.detailValue}>
                    {displayWem.avg_bitrate > 0
                      ? `${Math.round((displayWem.avg_bitrate * 8) / 1000)} kbps`
                      : "Unknown"}
                  </span>
                </div>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>Length</span>
                  <span className={styles.detailValue}>
                    {displayWem.avg_bitrate > 0
                      ? formatDuration(displayWem.size / displayWem.avg_bitrate)
                      : "Unknown"}
                  </span>
                </div>
                <div className={styles.detailRow}>
                  <span className={styles.detailLabel}>Size</span>
                  <span className={styles.detailValue}>
                    {displayWem.size > 0 ? formatBytes(displayWem.size) : "Unknown"}
                  </span>
                </div>
                {displayWem.isStreamed && (
                  <div className={styles.detailRow}>
                    <span className={styles.detailLabel}>Type</span>
                    <span className={styles.detailValue} style={{ color: "var(--warning)" }}>
                      Streamed (External)
                    </span>
                  </div>
                )}
              </div>

              <h3 className={styles.sectionTitle}>Audio Preview</h3>
              {audioLoading ? (
                <div className={styles.playerSection}>
                  <div className={styles.decodingText}>Decoding audio…</div>
                </div>
              ) : audioSrc ? (
                <CustomAudioPlayer
                  src={`data:audio/ogg;base64,${audioSrc}`}
                  wemId={displayWem.id}
                  autoPlay={true}
                />
              ) : (
                <div className={styles.playerSection}>
                  <div className={styles.decodingText}>Click to decode and play</div>
                </div>
              )}

              <h3 className={styles.sectionTitle}>Replace</h3>
              <div className={styles.extractGroup} style={{ marginBottom: "1rem" }}>
                <button
                  className={styles.extractBtn}
                  onClick={() => handleReplaceWem(displayWem.id)}
                  disabled={loading || displayWem.isStreamed}
                  title={displayWem.isStreamed ? "Replacing external streamed WEMs is not supported directly in the soundbank" : undefined}
                >
                  Replace Audio...
                </button>
              </div>

              <h3 className={styles.sectionTitle}>Extract</h3>
              <div className={styles.extractGroup}>
                <button
                  className={styles.extractBtn}
                  onClick={() => handleExtractWem([displayWem.id])}
                  disabled={loading}
                >
                  Extract This WEM
                </button>
                <button
                  className={styles.secondaryBtn}
                  onClick={() => handleExtractWem()}
                  disabled={loading}
                >
                  Extract All WEMs
                </button>
              </div>
            </>
          ) : (
            <div className={styles.emptyState}>Select a WEM to preview</div>
          )}
        </div>
      </div>

      <div className={styles.logContainer}>
        <StatusLog entries={log} />
      </div>

      {/* ── Batch Replace Modal ────────────────────────────── */}
      {showBatchModal && (() => {
        const bankWemIds = new Set(bankInfo?.wems.map((w) => w.id) || []);
        const matches: { id: number; path: string }[] = [];
        const ignored: { id: number; path: string }[] = [];

        Object.entries(scannedWems).forEach(([idStr, path]) => {
          const id = parseInt(idStr, 10);
          if (bankWemIds.has(id)) {
            matches.push({ id, path });
          } else {
            ignored.push({ id, path });
          }
        });

        return (
          <div className={styles.modalOverlay}>
            <div className={styles.modal}>
              <div className={styles.modalHeader}>
                <h3>Batch Replace WEMs</h3>
                <button
                  className={styles.closeBtn}
                  onClick={() => setShowBatchModal(false)}
                  disabled={injecting}
                >
                  ✕
                </button>
              </div>

              <div className={styles.modalBody}>
                <p style={{ margin: 0, fontSize: "0.85rem", color: "var(--text-secondary)" }}>
                  Select an input directory containing custom <code>.wem</code> files named with their target WEM ID (e.g., <code>12345.wem</code>).
                </p>

                <div className={styles.replaceGroup}>
                  <button
                    className={styles.secondaryBtn}
                    onClick={handleSelectBatchFolder}
                    disabled={injecting}
                  >
                    {batchFolder ? "Change Folder..." : "Select Folder..."}
                  </button>
                  {batchFolder && (
                    <div style={{ fontSize: "0.8rem", color: "var(--text-muted)", wordBreak: "break-all" }}>
                      <strong>Target Folder:</strong> {batchFolder}
                    </div>
                  )}
                </div>

                {batchFolder && (
                  <>
                    <div className={styles.scanStats}>
                      <div className={styles.statItem}>
                        <span className={styles.statLabel}>Matched WEMs</span>
                        <span className={`${styles.statValue} ${styles.matched}`}>{matches.length}</span>
                      </div>
                      <div className={styles.statItem}>
                        <span className={styles.statLabel}>Ignored Files</span>
                        <span className={`${styles.statValue} ${styles.ignored}`}>{ignored.length}</span>
                      </div>
                    </div>

                    {matches.length > 0 ? (
                      <>
                        <h4 style={{ margin: "0.5rem 0 0.25rem", fontSize: "0.85rem", fontWeight: 600 }}>
                          Matched Replacements:
                        </h4>
                        <div className={styles.matchList}>
                          {matches.map((m) => (
                            <div key={m.id} className={styles.matchRow}>
                              <span className={styles.matchId}>{m.id}.wem</span>
                              <span className={styles.matchPath} title={m.path}>
                                {m.path.split(/[\\/]/).pop()}
                              </span>
                            </div>
                          ))}
                        </div>
                      </>
                    ) : (
                      <div className={styles.emptyState} style={{ padding: "1rem" }}>
                        No matching WEM IDs found in the selected folder.
                      </div>
                    )}
                  </>
                )}
              </div>

              <div className={styles.modalFooter}>
                <button
                  className={styles.secondaryBtn}
                  style={{ width: "auto" }}
                  onClick={() => setShowBatchModal(false)}
                  disabled={injecting}
                >
                  Cancel
                </button>
                <button
                  className={styles.extractBtn}
                  style={{ width: "auto" }}
                  onClick={() => handleExecuteBatchInjection(matches)}
                  disabled={injecting || matches.length === 0}
                >
                  {injecting ? "Injecting..." : `Inject ${matches.length} WEM(s)`}
                </button>
              </div>
            </div>
          </div>
        );
      })()}
    </div>
  );
}
