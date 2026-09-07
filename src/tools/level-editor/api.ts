// Thin typed wrappers around the Level Editor Tauri commands.
//
// Every command takes camelCase arguments (Tauri converts the snake_case Rust
// parameter names for us). Errors surface as `ToolkitError` strings, so the
// callers just catch and forward the message into the status log.

import { invoke } from "@tauri-apps/api/core";
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
 * Results are memoised per model id — a zone reuses the same handful of models
 * thousands of times, so the viewport must never re-fetch one.
 */
export function makeMeshLoader(gameDir: string): MeshLoader {
  const cache = new Map<string, Promise<ArrayBuffer>>();
  return (modelId: string) => {
    let hit = cache.get(modelId);
    if (!hit) {
      hit = loadModelMesh(gameDir, modelId).catch((e) => {
        // A model that cannot be decoded must not poison the whole scene.
        console.warn(`level_model_mesh failed for ${modelId}:`, e);
        return new ArrayBuffer(0);
      });
      cache.set(modelId, hit);
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
