/**
 * Shared TypeScript types for the Level Editor / Viewer IPC contract.
 *
 * These mirror the Rust `#[tauri::command]` return shapes one-for-one.
 * Every command takes camelCase arguments (Tauri converts snake_case Rust
 * parameters to camelCase on the JS side).
 *
 * Numeric ids that do not fit in a JS number (`instanceId`, `modelId`,
 * `assetId`, `id`) are transported as lowercase `"0x…"` strings.
 */

export type Vec3 = [number, number, number];

/** A 4x4 transform, ROW-MAJOR, row 3 (indices 12,13,14) is the translation. */
export type Mat4Row = number[];

// ---------------------------------------------------------------------------
// level_list_planets
// ---------------------------------------------------------------------------

/** Closed set the backend uses for `LevelRegion.group`. */
export type LevelRegionGroup = "openarea" | "overlay" | "instance" | "other";

export interface LevelRegion {
  key: string;
  label: string;
  /** One of {@link LevelRegionGroup}. */
  group: string;
  zoneCount: number;
  /** Zone-kind histogram, e.g. `{ art: 12, gp: 4 }`. */
  kinds: Record<string, number>;
}

// ---------------------------------------------------------------------------
// level_list_zones
// ---------------------------------------------------------------------------

/** Closed set the backend uses for `ZoneEntry.kind`, derived from the file name. */
export type ZoneKind = "art" | "gp" | "lgt" | "vfx" | "nav" | "geo" | "other";

export interface ZoneEntry {
  /** Lowercase forward-slash asset path, e.g. `levels/i29/openarea/sargasso/tile_0_0/tile_0_0_art.zone`. */
  path: string;
  /** Asset id as a `"0x…"` string. */
  id: string;
  /** One of {@link ZoneKind}. */
  kind: string;
  /** Tile key (e.g. `"0_0"`) when the zone lives under a `tile_*` folder. */
  tile: string | null;
  size: number;
  hasObjects: boolean;
}

// ---------------------------------------------------------------------------
// level_load_zone
// ---------------------------------------------------------------------------

/** Scene-object kind constants (the top byte of `flags`). */
export const KIND_MODEL = 0;
export const KIND_LIGHT = 1;
export const KIND_ACTOR = 2;
export const KIND_DECAL = 3;
export const KIND_SPLINE = 4;
export const KIND_VFX = 5;

export const KIND_NAMES: Record<number, string> = {
  [KIND_MODEL]: "model",
  [KIND_LIGHT]: "light",
  [KIND_ACTOR]: "actor",
  [KIND_DECAL]: "decal",
  [KIND_SPLINE]: "spline",
  [KIND_VFX]: "vfx",
};

export function kindName(kind: number): string {
  return KIND_NAMES[kind] ?? `kind${kind}`;
}

export interface SceneObjectDto {
  /** Byte offset inside the zone scene block — the stable identity of the object. */
  offset: number;
  kind: number;
  kindName: string;
  size: number;
  flags: number;
  /** 16 floats, ROW-MAJOR, row 3 is the translation. */
  matrix: Mat4Row;
  scale: Vec3;
  /** `"0x…"` string. */
  instanceId: string;
  modelIndex: number;
  /** `"0x…"` string; `"0x0"` when the object has no model. */
  modelId: string;
  name: string;
  actorIndex: number;
}

export interface ZoneModelRef {
  /** `"0x…"` asset id, matches `SceneObjectDto.modelId`. */
  id: string;
  path: string;
  instanceCount: number;
}

export interface ZoneActorRow {
  index: number;
  sceneOffset: number;
  instanceId: string;
  name: string;
  assetId: string;
  assetPath: string;
}

export interface ZoneSection {
  tag: string;
  name: string;
  size: number;
}

export interface ZoneBounds {
  min: Vec3;
  max: Vec3;
}

export interface ZoneScene {
  path: string;
  id: string;
  sizes: number[];
  dirty: boolean;
  bounds: ZoneBounds | null;
  /** Unique models referenced by the zone, sorted by `instanceCount` descending. */
  models: ZoneModelRef[];
  /** Every scene object, including entities with no geometry. */
  objects: SceneObjectDto[];
  actors: ZoneActorRow[];
  sections: ZoneSection[];
}

// ---------------------------------------------------------------------------
// level_save_zone
// ---------------------------------------------------------------------------

export interface ZoneEdit {
  offset: number;
  /** 16 floats, ROW-MAJOR. */
  matrix?: Mat4Row;
  scale?: Vec3;
}

export interface ZoneSaveResult {
  assetPath: string;
  bytesWritten: number;
  projectFile: string;
}

// ---------------------------------------------------------------------------
// level_zone_stats
// ---------------------------------------------------------------------------

export interface LevelZoneStats {
  totalZones: number;
  regions: number;
  withObjects: number;
}

// ---------------------------------------------------------------------------
// Viewport
// ---------------------------------------------------------------------------

/** Loads the RCMS mesh buffer for a model asset id (`"0x…"`). */
export type MeshLoader = (modelId: string) => Promise<ArrayBuffer>;

export type GizmoMode = "translate" | "rotate" | "scale";
