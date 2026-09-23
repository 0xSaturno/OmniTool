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
  MaterialLoader,
  MeshLoader,
  ModelMaterialsDto,
  ModelMaterialSlotDto,
  TextureLoader,
  TextureSource,
  ZoneEdit,
  ZoneEntry,
  ZoneSaveResult,
  ZoneScene,
} from "./types";

export type {
  LevelRegion,
  LevelZoneStats,
  MaterialLoader,
  MeshLoader,
  ModelMaterialsDto,
  TextureLoader,
  TextureSource,
  ZoneEdit,
  ZoneEntry,
  ZoneSaveResult,
  ZoneScene,
};

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
  lastGameDir = gameDir;
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
/* Materials and textures                                              */
/* ------------------------------------------------------------------ */

/**
 * THE TEXTURING CONTRACT.
 *
 * `level_model_materials(gameDir, modelId) -> ModelMaterialsDto` (JSON)
 *   The model's material table, resolved down to the textures each slot binds.
 *   `slots[i].index` is the `materialIndex` the RCMS submesh header carries, so
 *   the viewport turns submeshes into geometry groups and indexes this array
 *   with nothing else. Nothing fails per slot: a material missing from the
 *   archives comes back `resolved: false, placeholder: true` with no textures,
 *   which is the viewport's cue to keep the flat per-model hue for that subset.
 *
 * `level_texture(gameDir, textureId, maxSize) -> Response` (binary)
 *   One texture decoded to a PNG. The mip served is the smallest resident mip
 *   at or above `maxSize` (the viewport asks for 1024 — see `textureCache.ts`
 *   for why); the streamed HD tiles are never fetched. A texture that cannot be
 *   found or decoded returns an EMPTY buffer rather than an error, so one
 *   unsupported format never aborts a zone's upload loop.
 *
 * `textureCache.ts` sniffs the payload rather than assuming PNG, so a future
 * backend that hands over the still-compressed BCn blocks (4-8x less VRAM, no
 * decode on either side) needs no change here.
 */

/**
 * The material table for one model.
 *
 * The backend never fails a slot, so the only defensive work is against a
 * missing/garbled reply, which degrades to "no materials" (flat hue) rather
 * than to a throw inside the render loop.
 */
export async function loadModelMaterials(gameDir: string, modelId: string): Promise<ModelMaterialsDto | null> {
  const raw = await invoke<unknown>("level_model_materials", { gameDir, modelId });
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (!Array.isArray(o.slots)) return null;
  return { modelId, slots: o.slots as ModelMaterialSlotDto[] };
}

export async function loadTexture(gameDir: string, textureId: string, maxSize: number): Promise<ArrayBuffer> {
  return invoke<ArrayBuffer>("level_texture", { gameDir, textureId, maxSize });
}

/**
 * The game directory the shell last built a mesh loader for.
 *
 * `LevelEditor.tsx` owns `gameDir` and hands the viewport exactly one function
 * (`makeMeshLoader`). Latching it here lets the texture source be built without
 * a new prop threaded through the shell, which is a file this change does not
 * own. When the shell next changes, pass `makeTextureSource(gameDir)` into
 * `<Viewport textureSource=…>` explicitly and this latch can go.
 */
let lastGameDir: string | null = null;

/**
 * Materials + textures bound to one game directory, with the material table
 * memoised per model id. A zone asks for the same model's materials once per
 * batch and a region reuses models across hundreds of zones, so without this
 * the command would be re-invoked thousands of times for identical answers.
 */
export function makeTextureSource(gameDir: string): TextureSource {
  const matCache = new Map<string, Promise<ModelMaterialsDto | null>>();
  const materials: MaterialLoader = (modelId) => {
    let hit = matCache.get(modelId);
    if (!hit) {
      hit = loadModelMaterials(gameDir, modelId).catch((e) => {
        // A model with no material table must not stop the rest of the zone
        // from texturing; it keeps the flat hue.
        console.warn(`materials failed for ${modelId}:`, e);
        return null;
      });
      matCache.set(modelId, hit);
    }
    return hit;
  };
  // Textures are deduplicated by the viewport's own shared cache, so no
  // memoisation here — one call per distinct texture id per process.
  const texture: TextureLoader = (id, maxSize) => loadTexture(gameDir, id, maxSize);
  return { materials, texture };
}

/** `makeTextureSource` for the directory the shell last used, or null. */
export function defaultTextureSource(): TextureSource | null {
  return lastGameDir ? makeTextureSource(lastGameDir) : null;
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
