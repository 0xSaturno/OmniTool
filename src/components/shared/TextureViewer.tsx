import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface TextureInfo {
  width: number;
  height: number;
  mipmaps: number;
  hdmipmaps: number;
  images: number;
  bytes_per_pixel: number;
  size: number;
  hdsize: number;
  format: number;
  is_cubemap: boolean;
  is_ibl: boolean;
  dimension: number;
  content_type: number;
}

export const FORMAT_MAP: Record<number, string> = {
  2: "R32G32B32A32_FLOAT",
  10: "R16G16B16A16_FLOAT",
  16: "R32G32_FLOAT",
  28: "R8G8B8A8_UNORM",
  29: "R8G8B8A8_UNORM_SRGB",
  61: "R8_UNORM",
  71: "BC1_UNORM",
  72: "BC1_UNORM_SRGB",
  74: "BC2_UNORM",
  75: "BC2_UNORM_SRGB",
  77: "BC3_UNORM",
  78: "BC3_UNORM_SRGB",
  80: "BC4_UNORM",
  83: "BC5_UNORM",
  87: "B8G8R8A8_UNORM",
  91: "B8G8R8A8_UNORM_SRGB",
  95: "BC6H_UF16",
  96: "BC6H_SF16",
  98: "BC7_UNORM",
  99: "BC7_UNORM_SRGB",
};

export const DIMENSION_MAP: Record<number, string> = {
  0: "1D",
  1: "2D",
  2: "3D",
  3: "Array",
  4: "Cube",
};

export function contentTypeLabels(ct: number): string[] {
  const labels: string[] = [];
  if (ct & 0x01) labels.push("sRGB");
  if (ct & 0x02) labels.push("Normal");
  if (ct & 0x04) labels.push("Param1");
  if (ct & 0x08) labels.push("IBL");
  if (ct & 0x10) labels.push("IES");
  if (ct & 0x20) labels.push("Param2");
  if (ct & 0x40) labels.push("Param3");
  return labels.length ? labels : ["Linear"];
}

// ─── Badge colours ────────────────────────────────────────────────────────────

const BADGE = {
  cube:  { bg: "#7c3aed22", fg: "#a78bfa", br: "#7c3aed55" },
  ibl:   { bg: "#d9770622", fg: "#fb923c", br: "#d9770655" },
  nrm:   { bg: "#05966922", fg: "#34d399", br: "#05966955" },
  srgb:  { bg: "#dc262622", fg: "#f87171", br: "#dc262655" },
  faces: { bg: "#0284c722", fg: "#38bdf8", br: "#0284c755" },
} as const;

function Badge({ label, style }: { label: string; style: typeof BADGE[keyof typeof BADGE] }) {
  return (
    <span style={{
      background: style.bg, color: style.fg,
      border: `1px solid ${style.br}`,
      borderRadius: "4px", padding: "0 0.35rem",
      fontSize: "0.68rem", fontWeight: 700,
    }}>
      {label}
    </span>
  );
}

/** Inline flag badges for a TextureInfo. Hover shows the raw content_type hex + label list. */
export function TextureBadges({ info, size = "normal" }: { info: TextureInfo; size?: "normal" | "small" }) {
  const ctLabels = contentTypeLabels(info.content_type);
  const ctTitle = `content_type 0x${info.content_type.toString(16).padStart(2, "0")}: ${ctLabels.join(", ")}`;
  const sz = size === "small" ? "0.65rem" : "0.68rem";
  return (
    <span style={{ display: "inline-flex", gap: "0.3rem", flexWrap: "wrap" }} title={ctTitle}>
      {info.is_cubemap && <Badge label="CUBE" style={{ ...BADGE.cube, bg: BADGE.cube.bg }} />}
      {info.is_ibl && <Badge label="IBL" style={BADGE.ibl} />}
      {(info.content_type & 0x02) !== 0 && <Badge label="NRM" style={BADGE.nrm} />}
      {(info.content_type & 0x01) !== 0 && <Badge label="sRGB" style={BADGE.srgb} />}
      {info.images > 1 && !info.is_cubemap && (
        <span style={{
          background: BADGE.faces.bg, color: BADGE.faces.fg,
          border: `1px solid ${BADGE.faces.br}`,
          borderRadius: "4px", padding: "0 0.35rem",
          fontSize: sz, fontWeight: 700,
        }}>{info.images} faces</span>
      )}
    </span>
  );
}

// ─── Async helpers ─────────────────────────────────────────────────────────────

/** Loads TextureInfo from a .texture file path. Returns null on error. */
export function useTextureInfo(path: string | null, type: "texture" | "dds" = "texture") {
  const [info, setInfo] = useState<TextureInfo | null>(null);
  useEffect(() => {
    if (!path) { setInfo(null); return; }
    let active = true;
    const cmd = type === "texture" ? "tauri_get_texture_info" : "tauri_get_dds_info";
    invoke<TextureInfo>(cmd, { path })
      .then(i => { if (active) setInfo(i); })
      .catch(() => { if (active) setInfo(null); });
    return () => { active = false; };
  }, [path, type]);
  return info;
}

/** Loads a base64 preview PNG for a texture or dds path. */
export function useTexturePreview(path: string | null, type: "texture" | "dds" = "texture") {
  const [preview, setPreview] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!path) { setPreview(null); setLoading(false); return; }
    let active = true;
    setLoading(true);
    setPreview(null);
    const cmd = type === "texture" ? "tauri_get_texture_preview" : "tauri_get_dds_preview";
    invoke<string>(cmd, { path })
      .then(p => { if (active) { setPreview(p); setLoading(false); } })
      .catch(() => { if (active) { setPreview(null); setLoading(false); } });
    return () => { active = false; };
  }, [path, type]);
  return { preview, loading };
}

// ─── TexturePreviewImage ───────────────────────────────────────────────────────

interface TexturePreviewImageProps {
  /** Path to a .texture or .dds file, or a pre-loaded base64 PNG string. */
  path?: string | null;
  /** If providing a pre-loaded base64 string, set preloaded=true. */
  preloaded?: boolean;
  preview?: string | null;
  loading?: boolean;
  type?: "texture" | "dds";
  style?: React.CSSProperties;
}

/**
 * A plain preview image box — pass either a file path (it loads internally)
 * or pass preloaded=true with preview/loading already resolved externally.
 */
export function TexturePreviewImage({
  path, preloaded, preview: extPreview, loading: extLoading, type = "texture", style,
}: TexturePreviewImageProps) {
  const { preview: intPreview, loading: intLoading } = useTexturePreview(
    preloaded ? null : (path ?? null),
    type,
  );
  const preview = preloaded ? extPreview : intPreview;
  const loading = preloaded ? !!extLoading : intLoading;

  return (
    <div style={{
      width: "100%", height: "100%",
      background: "var(--surface-2)",
      display: "flex", alignItems: "center", justifyContent: "center",
      color: "var(--text-muted)", fontSize: "0.8rem",
      borderRadius: "6px", overflow: "hidden",
      ...style,
    }}>
      {loading && <span>...</span>}
      {!loading && preview && (
        <img
          src={`data:image/png;base64,${preview}`}
          style={{ width: "100%", height: "100%", objectFit: "contain" }}
        />
      )}
      {!loading && !preview && <span style={{ fontSize: "0.7rem" }}>No preview</span>}
    </div>
  );
}

// ─── TextureInfoCard ───────────────────────────────────────────────────────────

/**
 * A vertical key-value card showing all texture metadata.
 * Designed for side-panels and detail views.
 */
export function TextureInfoCard({ info }: { info: TextureInfo }) {
  const rows: [string, string | number][] = [
    ["Resolution", `${info.width} × ${info.height}`],
    ["Dimension",  DIMENSION_MAP[info.dimension] ?? String(info.dimension)],
    ["Faces",      info.images],
    ["Format",     FORMAT_MAP[info.format] ?? `FMT_${info.format}`],
    ["Mips (SD/HD)", `${info.mipmaps} / ${info.hdmipmaps}`],
    ["Size (SD/HD)", `${(info.size / 1024).toFixed(1)} / ${(info.hdsize / 1024).toFixed(1)} KB`],
  ];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.4rem" }}>
      <div style={{ display: "flex", gap: "0.3rem", flexWrap: "wrap", marginBottom: "0.2rem" }}>
        <TextureBadges info={info} />
      </div>
      {rows.map(([label, value]) => (
        <div key={label} style={{ display: "flex", justifyContent: "space-between", fontSize: "0.8rem", gap: "0.5rem" }}>
          <span style={{ color: "var(--text-secondary)", flexShrink: 0 }}>{label}</span>
          <span style={{ color: "var(--text-primary)", fontWeight: 600, fontFamily: "var(--font-mono)", textAlign: "right", wordBreak: "break-all" }}>{value}</span>
        </div>
      ))}
    </div>
  );
}

// ─── TextureViewer ─────────────────────────────────────────────────────────────

interface TextureViewerProps {
  /** Path to a .texture or .dds file to load and display. */
  path: string | null;
  type?: "texture" | "dds";
  /** Show the metadata key-value card below the preview. Default true. */
  showInfo?: boolean;
  /** Show the section title header. Default true. */
  showTitle?: boolean;
  /** Height of the preview image box. Default "100%". */
  previewHeight?: string | number;
}

/**
 * All-in-one texture viewer: loads preview + info, shows badges, metadata rows.
 * Use this anywhere you want to display a .texture or .dds file inline.
 */
export function TextureViewer({
  path,
  type = "texture",
  showInfo = true,
  showTitle = true,
  previewHeight = "100%",
}: TextureViewerProps) {
  const { preview, loading } = useTexturePreview(path, type);
  const info = useTextureInfo(path, type);

  if (!path) return null;
  if (!loading && !preview && !info) return null;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.75rem" }}>
      {showTitle && (
        <h3 style={{
          fontSize: "0.85rem", fontWeight: 600, textTransform: "uppercase",
          letterSpacing: "0.05em", color: "var(--text-secondary)",
          margin: 0, borderBottom: "1px solid var(--border)", paddingBottom: "0.5rem",
        }}>
          Texture Preview
        </h3>
      )}

      <div style={{
        width: "100%", height: previewHeight,
        aspectRatio: preview ? undefined : "1 / 1",
        background: "var(--bg-surface)", borderRadius: "6px",
        border: "1px solid var(--border)",
        display: "flex", alignItems: "center", justifyContent: "center",
        overflow: "hidden",
      }}>
        {loading && <span style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>Loading…</span>}
        {preview && !loading && (
          <img src={`data:image/png;base64,${preview}`} style={{ width: "100%", height: "100%", objectFit: "contain" }} />
        )}
        {!preview && !loading && (
          <span style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>No preview</span>
        )}
      </div>

      {showInfo && info && <TextureInfoCard info={info} />}
    </div>
  );
}

// ─── TocTextureViewer ──────────────────────────────────────────────────────────

interface TocTextureViewerProps {
  assetPath: string;
  tocPath: string;
  assetId: string;
  archivesDir: string;
  showTitle?: boolean;
}

/**
 * Like TextureViewer but extracts the asset from the TOC to a temp file first.
 * Used by the Asset Browser details panel.
 */
export function TocTextureViewer({ assetPath, tocPath, assetId, archivesDir, showTitle = true }: TocTextureViewerProps) {
  const [tempPath, setTempPath] = useState<string | null>(null);
  const [extracting, setExtracting] = useState(false);
  const cacheKey = `${tocPath}::${assetId}`;

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
  }, [cacheKey, tocPath, assetId, archivesDir, assetPath]);

  if (!extracting && !tempPath) return null;

  if (extracting) {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: "0.75rem" }}>
        {showTitle && (
          <h3 style={{ fontSize: "0.85rem", fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.05em", color: "var(--text-secondary)", margin: 0, borderBottom: "1px solid var(--border)", paddingBottom: "0.5rem" }}>
            Texture Preview
          </h3>
        )}
        <div style={{ width: "100%", aspectRatio: "1 / 1", background: "var(--bg-surface)", borderRadius: "6px", border: "1px solid var(--border)", display: "flex", alignItems: "center", justifyContent: "center" }}>
          <span style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>Extracting…</span>
        </div>
      </div>
    );
  }

  return <TextureViewer path={tempPath} type="texture" showTitle={showTitle} />;
}
