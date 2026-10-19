// Thin typed wrappers around the Level Editor Tauri commands.
//
// Every command takes camelCase arguments (Tauri converts the snake_case Rust
// parameter names for us). Errors surface as `ToolkitError` strings, so the
// callers just catch and forward the message into the status log.

import { invoke } from "@tauri-apps/api/core";
import { parseZoneGeometryKey } from "./types";
import type {
  ActorSheetsDto,
  AtmosphereLightingDto,
  LevelRegion,
  LevelZoneStats,
  MaterialLoader,
  MeshLoader,
  ModelMaterialsDto,
  ModelMaterialSlotDto,
  PriusEditPlan,
  PriusSchemaStatus,
  PriusSheetDto,
  ScriptGraphDto,
  ScriptNodeDetailDto,
  StagedPriusEdit,
  TextureLoader,
  TextureSource,
  TuidTarget,
  ZoneEdit,
  ZoneEntry,
  ZoneLightingDto,
  ZonePriusSummary,
  ZoneSaveResult,
  ZoneScene,
} from "./types";

export type {
  ActorSheetsDto,
  AtmosphereLightingDto,
  LevelRegion,
  LevelZoneStats,
  MaterialLoader,
  MeshLoader,
  ModelMaterialsDto,
  PriusEditPlan,
  PriusSchemaStatus,
  PriusSheetDto,
  ScriptGraphDto,
  ScriptNodeDetailDto,
  StagedPriusEdit,
  TextureLoader,
  TextureSource,
  TuidTarget,
  ZoneEdit,
  ZoneEntry,
  ZoneLightingDto,
  ZonePriusSummary,
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

/* ------------------------------------------------------------------ */
/* Decoded gameplay data: priuses, scripts, lights                     */
/*                                                                     */
/* All of these are READ-ONLY against the shipped archives. The one     */
/* exception is `planPriusEdit`, which still writes nothing: it type-   */
/* checks one field edit and hands back an equal-length byte patch for  */
/* the zone session to splice in.                                      */
/* ------------------------------------------------------------------ */

/**
 * The game directory the shell last built a mesh loader for.
 *
 * `LevelEditor.tsx` owns `gameDir` and does not thread it into `Inspector`,
 * so the panels that need their own round trip read it from here. It is
 * latched by `makeMeshLoader`, which the shell calls in a `useMemo` on the
 * same value, so it is set before any zone can be selected.
 */
export function currentGameDir(): string | null {
  return lastGameDir;
}

export async function priusSchemaStatus(): Promise<PriusSchemaStatus> {
  return invoke<PriusSchemaStatus>("prius_schema_status");
}

/**
 * {@link priusSchemaStatus}, asked once per process.
 *
 * WHY A PANEL HAS TO ASK AT ALL. The decoder works without `rcra_ddl.json` —
 * field names come out of the zone's own string pool either way — but
 * everything that makes a property sheet a property sheet comes from the type
 * database, and a sheet without it looks *plausible* rather than broken, which
 * is the worst way for a tool to be wrong. MEASURED over the 45 component
 * priuses (163 editable leaves) of the 24 actors this change was proven on,
 * in `i29_molonoth_gp.zone`:
 *
 * |                              | schema loaded | no schema |
 * |------------------------------|---------------|-----------|
 * | priuses naming their DDL type|         45/45 |      0/45 |
 * | leaves with a designer label |       116/163 |     0/163 |
 * | leaves with a schema type    |        87/163 |     0/163 |
 * | leaves with enum members     |        39/163 |     0/163 |
 *
 * So without it `SpinAxis` is a text box reading `kZAxisLocal` instead of a
 * six-member dropdown, and `MovementConfig` is free text instead of an asset
 * path. {@link PriusSchemaStatus.searched} carries the paths the backend
 * tried, which is what the user needs to fix it.
 *
 * Memoised because the answer cannot change without a `prius_load_schema`
 * call, and the Properties tab would otherwise ask on every selection.
 */
let schemaStatusOnce: Promise<PriusSchemaStatus> | null = null;
export function priusSchemaStatusCached(): Promise<PriusSchemaStatus> {
  if (!schemaStatusOnce) {
    schemaStatusOnce = priusSchemaStatus().catch((e) => {
      schemaStatusOnce = null;
      throw e;
    });
  }
  return schemaStatusOnce;
}

/** Forget the memoised status — call after {@link loadPriusSchema}. */
export function invalidatePriusSchemaStatus(): void {
  schemaStatusOnce = null;
}

export async function loadPriusSchema(path: string): Promise<PriusSchemaStatus> {
  return invoke<PriusSchemaStatus>("prius_load_schema", { path });
}

export async function priusZoneSummary(
  gameDir: string,
  zonePath: string,
): Promise<ZonePriusSummary> {
  return invoke<ZonePriusSummary>("prius_zone_summary", { gameDir, zonePath });
}

/** Every component prius of one placed actor, decoded into named fields. */
export async function priusActorSheet(
  gameDir: string,
  zonePath: string,
  actorIndex: number,
): Promise<ActorSheetsDto> {
  return invoke<ActorSheetsDto>("prius_actor_sheet", { gameDir, zonePath, actorIndex });
}

/** The prius of one script action, by its `Zone Script Actions` row index. */
export async function priusScriptSheet(
  gameDir: string,
  zonePath: string,
  actionIndex: number,
): Promise<PriusSheetDto> {
  return invoke<PriusSheetDto>("prius_script_sheet", { gameDir, zonePath, actionIndex });
}

/**
 * Ask the backend whether one field may take one value, and for the bytes.
 *
 * Pass `number` for anything numeric or boolean and `text` for a select member
 * or a free string — never both. A refusal comes back as a RESOLVED plan with
 * `ok: false` and a human `error`, not as a throw; only a broken zone path or
 * a missing archive throws.
 */
export async function planPriusEdit(args: {
  gameDir: string;
  zonePath: string;
  row: number;
  fieldPath: string;
  number?: number;
  text?: string;
  scriptAction?: boolean;
}): Promise<PriusEditPlan> {
  return invoke<PriusEditPlan>("prius_plan_edit", {
    gameDir: args.gameDir,
    zonePath: args.zonePath,
    row: args.row,
    fieldPath: args.fieldPath,
    number: args.number,
    text: args.text,
    scriptAction: args.scriptAction ?? false,
  });
}

/**
 * One zone's script graph.
 *
 * `subgraph` restricts the node list to one connected component and
 * `nodeLimit` caps it — the largest shipped graph is 14 707 nodes in
 * `i29_molonoth_gp.zone`, so the caller picks a component before paying for
 * it. The subgraph index always comes back in full.
 *
 * NOTE: when either narrowing trims the node list the returned `edges` array
 * is filtered too, which invalidates the `inEdges`/`outEdges` indices the
 * nodes carry. Use {@link zoneScriptNode} for a node's actual edges.
 */
export async function zoneScriptGraph(
  gameDir: string,
  zonePath: string,
  subgraph?: number,
  nodeLimit?: number,
): Promise<ScriptGraphDto> {
  return invoke<ScriptGraphDto>("level_zone_script_graph", {
    gameDir,
    zonePath,
    subgraph,
    nodeLimit,
  });
}

/** One node with its edges resolved, its bound variables and its prius blob. */
export async function zoneScriptNode(
  gameDir: string,
  zonePath: string,
  nodeIndex: number,
): Promise<ScriptNodeDetailDto> {
  return invoke<ScriptNodeDetailDto>("level_zone_script_node", { gameDir, zonePath, nodeIndex });
}

export async function zoneScriptDump(
  gameDir: string,
  zonePath: string,
  subgraph?: number,
): Promise<string> {
  return invoke<string>("level_zone_script_dump", { gameDir, zonePath, subgraph });
}

export async function zoneLighting(
  gameDir: string,
  zonePath: string,
  condition?: number,
): Promise<ZoneLightingDto> {
  return invoke<ZoneLightingDto>("level_zone_lighting", { gameDir, zonePath, condition });
}

export async function atmosphereLighting(
  gameDir: string,
  atmosphere: string,
): Promise<AtmosphereLightingDto> {
  return invoke<AtmosphereLightingDto>("level_atmosphere_lighting", { gameDir, atmosphere });
}

/* ------------------------------------------------------------------ */
/* Per-zone caches for the inspector                                   */
/*                                                                     */
/* The inspector re-fetches on every selection change, and a selection  */
/* change is one click. These keep the cost at one round trip per zone  */
/* rather than one per click. Each is capped so that a bulk load of a    */
/* whole planet cannot grow them without bound.                        */
/* ------------------------------------------------------------------ */

function cappedCache<T>(limit: number) {
  const map = new Map<string, Promise<T>>();
  return (key: string, make: () => Promise<T>): Promise<T> => {
    const hit = map.get(key);
    if (hit) return hit;
    const p = make();
    map.set(key, p);
    // Drop the oldest insertion once over the cap. Map preserves insert order.
    while (map.size > limit) {
      const oldest = map.keys().next();
      if (oldest.done) break;
      map.delete(oldest.value);
    }
    // A rejected fetch must not be remembered, or the panel can never retry.
    p.catch(() => map.delete(key));
    return p;
  };
}

const lightingCache = cappedCache<ZoneLightingDto>(8);
const tuidCache = cappedCache<Map<string, TuidTarget>>(8);

/** {@link zoneLighting}, memoised per (game dir, zone, condition). */
export function zoneLightingCached(
  gameDir: string,
  zonePath: string,
  condition = 0,
): Promise<ZoneLightingDto> {
  return lightingCache(`${gameDir}|${zonePath}|${condition}`, () =>
    zoneLighting(gameDir, zonePath, condition),
  );
}

/** A u64 tuid, however it arrived (decimal string, `0x…`, number), as decimal. */
export function normaliseTuid(v: string | number): string {
  try {
    const s = typeof v === "number" ? BigInt(Math.trunc(v)) : BigInt(v);
    return (s < 0n ? s + (1n << 64n) : s).toString();
  } catch {
    return String(v);
  }
}

/**
 * `tuid -> the actor or scene object it names`, for one zone.
 *
 * Built once per zone from `level_load_zone` and then thrown away except for
 * the compact map, so a prius sheet can turn a `tuid` leaf into a link instead
 * of a 16-digit number. Actors win over scene objects when both carry the uid,
 * because an actor row is the thing a designer means.
 */
export function zoneTuidIndex(gameDir: string, zonePath: string): Promise<Map<string, TuidTarget>> {
  return tuidCache(`${gameDir}|${zonePath}`, async () => {
    const scene = await loadZone(gameDir, zonePath);
    const out = new Map<string, TuidTarget>();
    for (const o of scene.objects) {
      const key = normaliseTuid(o.instanceId);
      if (key === "0") continue;
      out.set(key, { kind: "object", name: o.name, offset: o.offset, kindName: o.kindName });
    }
    for (const a of scene.actors) {
      const key = normaliseTuid(a.instanceId);
      if (key === "0") continue;
      out.set(key, {
        kind: "actor",
        name: a.name || a.assetPath || a.assetId,
        actorIndex: a.index,
        offset: a.sceneOffset,
      });
    }
    return out;
  });
}

/* ------------------------------------------------------------------ */
/* Staged prius edits                                                  */
/* ------------------------------------------------------------------ */

/**
 * Prius edits the user has made and the backend has approved, waiting to be
 * written.
 *
 * WHY THEY WAIT. `level_save_zone` takes `ZoneEditDto { offset, matrix, scale }`
 * — it can only rewrite a scene object's transform. A prius patch is a
 * different shape: an equal-length byte range inside `Zone Actor Prius Data`
 * or `Zone Script Priuses`. Until `level_save_zone` accepts that shape (the
 * exact patch is in this change's handoff), staging them here is the honest
 * thing to do: the panel shows them as pending and says, in the UI, that the
 * save path does not carry them yet, rather than dropping them silently or
 * pretending they were written.
 *
 * The store is deliberately module-level rather than React state: the shell
 * owns the transform edit reducer and this change does not touch the shell.
 */
const stagedPriusEdits = new Map<string, StagedPriusEdit>();
const stagedListeners = new Set<() => void>();
let stagedSnapshot: StagedPriusEdit[] = [];

function priusEditKey(e: {
  zonePath: string;
  row: number;
  scriptAction: boolean;
  fieldPath: string;
}) {
  return `${e.zonePath}|${e.scriptAction ? "s" : "a"}${e.row}|${e.fieldPath}`;
}

function bumpStaged() {
  stagedSnapshot = [...stagedPriusEdits.values()];
  for (const fn of stagedListeners) fn();
}

export function subscribeStagedPriusEdits(fn: () => void): () => void {
  stagedListeners.add(fn);
  return () => stagedListeners.delete(fn);
}

/** Stable-identity snapshot, safe for `useSyncExternalStore`. */
export function stagedPriusEditList(): StagedPriusEdit[] {
  return stagedSnapshot;
}

export function stagePriusEdit(edit: StagedPriusEdit): void {
  // Re-editing a field back to the bytes it shipped with is not an edit.
  const key = priusEditKey(edit);
  const same =
    edit.bytes.length === edit.oldBytes.length &&
    edit.bytes.every((b, i) => b === edit.oldBytes[i]);
  if (same) stagedPriusEdits.delete(key);
  else stagedPriusEdits.set(key, edit);
  bumpStaged();
}

export function unstagePriusEdit(e: {
  zonePath: string;
  row: number;
  scriptAction: boolean;
  fieldPath: string;
}): void {
  if (stagedPriusEdits.delete(priusEditKey(e))) bumpStaged();
}

export function clearStagedPriusEdits(zonePath?: string): void {
  if (!zonePath) {
    if (stagedPriusEdits.size === 0) return;
    stagedPriusEdits.clear();
  } else {
    let hit = false;
    for (const [k, v] of stagedPriusEdits) {
      if (v.zonePath === zonePath) {
        stagedPriusEdits.delete(k);
        hit = true;
      }
    }
    if (!hit) return;
  }
  bumpStaged();
}
