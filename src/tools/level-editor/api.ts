// Thin typed wrappers around the Level Editor Tauri commands.
//
// Every command takes camelCase arguments (Tauri converts the snake_case Rust
// parameter names for us). Errors surface as `ToolkitError` strings, so the
// callers just catch and forward the message into the status log.

import { invoke } from "@tauri-apps/api/core";
import { parseZoneGeometryKey } from "./types";
import type {
  LevelRegion,
  LevelZoneStats,
  MeshLoader,
  ZoneEdit,
  ZoneEntry,
  ZoneSaveResult,
  ZoneScene,
} from "./types";

export type { LevelRegion, LevelZoneStats, MeshLoader, ZoneEdit, ZoneEntry, ZoneSaveResult, ZoneScene };

export async function listPlanets(gameDir: string, hashesPath?: string): Promise<LevelRegion[]> {
  return invoke<LevelRegion[]>("level_list_planets", { gameDir, hashesPath });
}

export async function listZones(
  gameDir: string,
  region: string,
  hashesPath?: string,
): Promise<ZoneEntry[]> {
  return invoke<ZoneEntry[]>("level_list_zones", { gameDir, region, hashesPath });
}

export async function loadZone(
  gameDir: string,
  zonePath: string,
  hashesPath?: string,
): Promise<ZoneScene> {
  return invoke<ZoneScene>("level_load_zone", { gameDir, zonePath, hashesPath });
}

export async function loadModelMesh(
  gameDir: string,
  modelId: string,
  lod?: number,
): Promise<ArrayBuffer> {
  return invoke<ArrayBuffer>("level_model_mesh", { gameDir, modelId, lod });
}

/**
 * RCMS buffer for the mesh one scene object keeps inside the zone's own raw
 * block 2 — kind 6 impostor shells and kind 3 projected decal sheets, neither
 * of which has a `.model` asset. Vertices come back in the entry's local
 * space, so the viewport places them with the entry's own 4x4.
 *
 * Returns the header-only empty buffer (never an error) when the object has no
 * block-2 span or the span will not decode.
 */
export async function loadZoneGeometry(
  gameDir: string,
  zonePath: string,
  offset: number,
): Promise<ArrayBuffer> {
  return invoke<ArrayBuffer>("level_zone_geometry", { gameDir, zonePath, offset });
}

export async function saveZone(
  gameDir: string,
  zonePath: string,
  projectName: string,
  edits: ZoneEdit[],
): Promise<ZoneSaveResult> {
  return invoke<ZoneSaveResult>("level_save_zone", { gameDir, zonePath, projectName, edits });
}

export async function zoneStats(gameDir: string, hashesPath?: string): Promise<LevelZoneStats> {
  return invoke<LevelZoneStats>("level_zone_stats", { gameDir, hashesPath });
}

/**
 * Builds the `meshLoader` the viewport calls, bound to one game directory.
 *
 * One loader serves both mesh sources, chosen by the shape of the key:
 *   `"0x…"`                     -> `level_model_mesh` (a shared `.model` asset)
 *   `"zone:<zonePath>#<offset>"` -> `level_zone_geometry` (block-2 impostor/decal)
 *
 * Keeping them in one keyspace lets the viewport run a single progressive
 * queue over a single geometry cache, and lets the shell keep handing down one
 * function. Results are memoised per key — a zone reuses the same handful of
 * models thousands of times, so the viewport must never re-fetch one, and a
 * block-2 mesh is per-object so it must be fetched exactly once.
 */
export function makeMeshLoader(gameDir: string): MeshLoader {
  const cache = new Map<string, Promise<ArrayBuffer>>();
  return (key: string) => {
    let hit = cache.get(key);
    if (!hit) {
      const zone = parseZoneGeometryKey(key);
      const call = zone
        ? loadZoneGeometry(gameDir, zone.zonePath, zone.offset)
        : loadModelMesh(gameDir, key);
      hit = call.catch((e) => {
        // One undecodable mesh must not poison the whole scene; the viewport
        // keeps its placeholder box for an empty buffer.
        console.warn(`mesh load failed for ${key}:`, e);
        return new ArrayBuffer(0);
      });
      cache.set(key, hit);
    }
    return hit;
  };
}

/* ------------------------------------------------------------------ */
/* Small shared helpers used by more than one panel                    */
/* ------------------------------------------------------------------ */

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** `levels/i29/openarea/sargasso/tile_1_2/tile_1_2_art.zone` -> `tile_1_2_art.zone` */
export function zoneFileName(path: string): string {
  return path.split("/").pop() ?? path;
}

export const ZONE_KINDS = ["art", "gp", "lgt", "vfx", "nav", "geo", "other"] as const;

export const KIND_LABELS: Record<string, string> = {
  art: "Art",
  gp: "Gameplay",
  lgt: "Lighting",
  vfx: "VFX",
  nav: "Nav",
  geo: "Geo",
  other: "Other",
};
