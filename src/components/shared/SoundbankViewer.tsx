import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useNavigate } from "react-router-dom";

export interface SoundbankEvent {
  id: number;
  name: string;
}

export interface SoundbankMetadata {
  bank_id: number;
  bank_name: string;
  bnk_size: number;
  events: SoundbankEvent[];
}

export interface SoundbankViewerProps {
  path: string | null;
  showTitle?: boolean;
}

export function SoundbankViewer({ path, showTitle = true }: SoundbankViewerProps) {
  const navigate = useNavigate();
  const [info, setInfo] = useState<SoundbankMetadata | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    if (!path) {
      setInfo(null);
      setError(null);
      return;
    }

    let active = true;
    setLoading(true);
    setError(null);

    invoke<SoundbankMetadata>("tauri_get_soundbank_info", { path })
      .then((metadata) => {
        if (active) {
          setInfo(metadata);
        }
      })
      .catch((err) => {
        if (active) {
          setError(String(err));
        }
      })
      .finally(() => {
        if (active) {
          setLoading(false);
        }
      });

    return () => {
      active = false;
    };
  }, [path]);

  if (!path) return null;

  const filteredEvents = info?.events.filter((e) =>
    e.name.toLowerCase().includes(filter.toLowerCase()) ||
    e.id.toString().includes(filter)
  ) ?? [];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.75rem", width: "100%" }}>
      {showTitle && (
        <h3 style={{
          fontSize: "0.85rem", fontWeight: 600, textTransform: "uppercase",
          letterSpacing: "0.05em", color: "var(--text-secondary)",
          margin: 0, borderBottom: "1px solid var(--border)", paddingBottom: "0.5rem",
        }}>
          Soundbank Inspector
        </h3>
      )}

      {loading && (
        <div style={{ padding: "1rem", textAlign: "center", color: "var(--text-muted)", fontSize: "0.8rem" }}>
          Reading soundbank...
        </div>
      )}

      {error && (
        <div style={{ padding: "1rem", color: "red", background: "rgba(255, 0, 0, 0.1)", borderRadius: "6px", fontSize: "0.8rem" }}>
          Error parsing soundbank: {error}
        </div>
      )}

      {!loading && !error && info && (
        <div style={{ display: "flex", flexDirection: "column", gap: "0.8rem" }}>
          {/* Metadata Card */}
          <div style={{ display: "flex", flexDirection: "column", gap: "0.4rem", padding: "0.75rem", background: "var(--surface-2, #1e1e24)", border: "1px solid var(--border, #333)", borderRadius: "6px" }}>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Bank ID</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)" }}>{info.bank_id}</span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>BNK Payload Size</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)" }}>
                {(info.bnk_size / 1024).toFixed(2)} KB
              </span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Registered Events</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600 }}>{info.events.length}</span>
            </div>
            <button
              onClick={() => navigate("/tools/bnk-explorer", { state: { filePath: path } })}
              style={{
                background: "var(--accent, #a78bfa)",
                color: "#000",
                border: "none",
                borderRadius: "4px",
                padding: "0.4rem 0.75rem",
                fontSize: "0.75rem",
                fontWeight: 600,
                marginTop: "0.4rem",
                cursor: "pointer",
                textAlign: "center",
                transition: "opacity 0.15s",
              }}
              onMouseOver={(e) => (e.currentTarget.style.opacity = "0.85")}
              onMouseOut={(e) => (e.currentTarget.style.opacity = "1")}
            >
              Open in BNK Explorer
            </button>
          </div>

          {/* Events List */}
          {info.events.length > 0 ? (
            <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
              <div style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
                <input
                  type="text"
                  placeholder="Filter events..."
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  style={{
                    flex: 1,
                    background: "var(--surface-1, #121214)",
                    border: "1px solid var(--border, #333)",
                    borderRadius: "4px",
                    padding: "0.35rem 0.5rem",
                    color: "var(--text-primary)",
                    fontSize: "0.75rem",
                  }}
                />
              </div>
              
              <div style={{
                maxHeight: "220px",
                overflowY: "auto",
                border: "1px solid var(--border, #333)",
                borderRadius: "6px",
                background: "var(--surface-1, #121214)",
              }}>
                <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "0.75rem" }}>
                  <thead>
                    <tr style={{ background: "var(--surface-2, #1e1e24)", borderBottom: "1px solid var(--border, #333)" }}>
                      <th style={{ textAlign: "left", padding: "0.4rem 0.6rem", color: "var(--text-secondary)" }}>Event Name</th>
                      <th style={{ textAlign: "right", padding: "0.4rem 0.6rem", color: "var(--text-secondary)", width: "100px" }}>FNV-1a Hash</th>
                    </tr>
                  </thead>
                  <tbody>
                    {filteredEvents.map((e, idx) => (
                      <tr key={idx} style={{ borderBottom: "1px solid var(--border, #333)", height: "26px" }}>
                        <td style={{ padding: "0.3rem 0.6rem", color: "var(--text-primary)", fontWeight: 500, wordBreak: "break-all" }}>{e.name}</td>
                        <td style={{ padding: "0.3rem 0.6rem", color: "var(--text-muted)", fontFamily: "var(--font-mono)", textAlign: "right" }}>
                          0x{e.id.toString(16).toUpperCase()}
                        </td>
                      </tr>
                    ))}
                    {filteredEvents.length === 0 && (
                      <tr>
                        <td colSpan={2} style={{ padding: "1rem", textAlign: "center", color: "var(--text-muted)" }}>
                          No events found
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
            </div>
          ) : (
            <div style={{ padding: "1rem", textAlign: "center", color: "var(--text-muted)", fontSize: "0.8rem", fontStyle: "italic" }}>
              No events registered in this soundbank
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export interface TocSoundbankViewerProps {
  assetPath: string;
  tocPath: string;
  assetId: string;
  archivesDir: string;
  showTitle?: boolean;
}

export function TocSoundbankViewer({ assetPath, tocPath, assetId, archivesDir, showTitle = true }: TocSoundbankViewerProps) {
  const [tempPath, setTempPath] = useState<string | null>(null);
  const [extracting, setExtracting] = useState(false);

  useEffect(() => {
    if (!tocPath || !assetId || !archivesDir) return;

    let active = true;
    setTempPath(null);
    setExtracting(true);

    invoke<string>("extract_to_temp", { tocPath, assetId, archivesDir, filename: assetPath })
      .then(p => { if (active) setTempPath(p); })
      .catch(() => {})
      .finally(() => { if (active) setExtracting(false); });

    return () => { active = false; };
  }, [tocPath, assetId, archivesDir, assetPath]);

  if (!extracting && !tempPath) return null;

  if (extracting) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: "0.75rem" }}>
        {showTitle && (
          <h3 style={{ fontSize: "0.85rem", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.05em", color: "var(--text-secondary)", margin: 0, borderBottom: "1px solid var(--border)", paddingBottom: "0.5rem" }}>
            Soundbank Inspector
          </h3>
        )}
        <div style={{ width: "100%", padding: "1rem", background: "var(--surface-1, #121214)", borderRadius: "6px", border: "1px solid var(--border, #333)", display: "flex", alignItems: "center", justifyContent: "center" }}>
          <span style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>Extracting soundbank…</span>
        </div>
      </div>
    );
  }

  return <SoundbankViewer path={tempPath} showTitle={showTitle} />;
}
