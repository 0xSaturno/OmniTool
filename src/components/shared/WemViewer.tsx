import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import CustomAudioPlayer from "./CustomAudioPlayer";

export interface WemFullInfo {
  codec: string;
  codec_id: number;
  sample_rate: number;
  channels: number;
  avg_bitrate: number;
  size: number;
  audio_src: string | null;
}

export interface TocWemViewerProps {
  assetPath: string;
  tocPath: string;
  assetId: string;
  archivesDir: string;
  sourceMode?: string;
  showTitle?: boolean;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
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

export function TocWemViewer({
  assetPath,
  tocPath,
  assetId,
  archivesDir,
  sourceMode = "live",
  showTitle = true,
}: TocWemViewerProps) {
  const [info, setInfo] = useState<WemFullInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!tocPath || !assetId || !archivesDir) return;

    let active = true;
    setLoading(true);
    setError(null);
    setInfo(null);

    invoke<WemFullInfo>("wem_get_info_and_preview", {
      tocPath,
      assetId,
      archivesDir,
      sourceMode,
      filename: assetPath,
    })
      .then((res) => {
        if (active) {
          setInfo(res);
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
  }, [tocPath, assetId, archivesDir, sourceMode]);

  if (!tocPath || !assetId || !archivesDir) return null;

  const duration = info && info.avg_bitrate > 0 ? info.size / info.avg_bitrate : 0;
  const durationStr = duration > 0 ? formatDuration(duration) : "Unknown";

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.75rem", width: "100%" }}>
      {showTitle && (
        <h3 style={{
          fontSize: "0.85rem", fontWeight: 600, textTransform: "uppercase",
          letterSpacing: "0.05em", color: "var(--text-secondary)",
          margin: 0, borderBottom: "1px solid var(--border)", paddingBottom: "0.5rem",
        }}>
          WEM Inspector
        </h3>
      )}

      {loading && (
        <div style={{ padding: "1rem", textAlign: "center", color: "var(--text-muted)", fontSize: "0.8rem" }}>
          Reading WEM...
        </div>
      )}

      {error && (
        <div style={{ padding: "1rem", color: "red", background: "rgba(255, 0, 0, 0.1)", borderRadius: "6px", fontSize: "0.8rem" }}>
          Error parsing WEM: {error}
        </div>
      )}

      {!loading && !error && info && (
        <div style={{ display: "flex", flexDirection: "column", gap: "0.8rem" }}>
          {/* Metadata Card */}
          <div style={{
            display: "flex",
            flexDirection: "column",
            gap: "0.4rem",
            padding: "0.75rem",
            background: "var(--surface-2, #1e1e24)",
            border: "1px solid var(--border, #333)",
            borderRadius: "6px"
          }}>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Codec</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600 }}>{info.codec}</span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Sample Rate</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)" }}>
                {info.sample_rate} Hz
              </span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Channels</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600 }}>{info.channels}</span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Bitrate</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)" }}>
                {info.avg_bitrate > 0 ? `${Math.round((info.avg_bitrate * 8) / 1000)} kbps` : "Unknown"}
              </span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Length</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)" }}>
                {durationStr}
              </span>
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem" }}>
              <span style={{ color: "var(--text-secondary)" }}>Size</span>
              <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)" }}>
                {formatBytes(info.size)}
              </span>
            </div>
          </div>

          {/* Audio Preview section */}
          <div style={{
            display: "flex",
            flexDirection: "column",
            gap: "0.5rem",
            width: "100%",
            boxSizing: "border-box"
          }}>
            <span style={{ fontSize: "0.7rem", fontWeight: 600, textTransform: "uppercase", color: "var(--text-secondary)", letterSpacing: "0.05em", alignSelf: "flex-start", marginBottom: "0.25rem" }}>
              Audio Preview
            </span>
            {info.audio_src ? (
              <CustomAudioPlayer
                src={`data:audio/ogg;base64,${info.audio_src}`}
                wemId={assetId}
                autoPlay={true}
              />
            ) : (
              <div style={{
                fontSize: "0.75rem",
                color: "var(--text-muted)",
                fontStyle: "italic",
                padding: "1rem",
                width: "100%",
                textAlign: "center",
                background: "var(--surface-1, #121214)",
                border: "1px solid var(--border, #2d2d30)",
                borderRadius: "6px"
              }}>
                Preview unavailable (prefetched header / truncated or unsupported codec)
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
