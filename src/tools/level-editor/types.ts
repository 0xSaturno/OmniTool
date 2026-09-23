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
export const KIND_IMPOSTOR = 6;

export const KIND_NAMES: Record<number, string> = {
  [KIND_MODEL]: "model",
  [KIND_LIGHT]: "light",
  [KIND_ACTOR]: "actor",
  [KIND_DECAL]: "decal",
  [KIND_SPLINE]: "spline",
  [KIND_VFX]: "vfx",
  [KIND_IMPOSTOR]: "impostor",
};

export function kindName(kind: number): string {
  return KIND_NAMES[kind] ?? `kind${kind}`;
}

/**
 * Where a scene-object entry's own mesh lives inside the zone's raw block 2.
 *
 * Only kind 3 (projected decal sheet) and kind 6 (baked impostor shell) carry
 * one — no `.model` asset exists for either, so `level_zone_geometry` is the
 * only route to those triangles. Present iff the backend found a well-formed
 * span, so its mere presence is the cheap "this object has a mesh" signal the
 * viewport needs in order to avoid one IPC round trip per boxy entity.
 */
export interface GeoRef {
  vertexOffset: number;
  vertexCount: number;
  indexOffset: number;
  indexCount: number;
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
  /** Kind 3: the inline `.material` path. Kind 6: the inline `.texture` atlas. */
  assetPath?: string;
  /** Block-2 mesh span; absent on every kind that has none. See {@link GeoRef}. */
  geometry?: GeoRef | null;
  /**
   * Mesh-loader key for this object's own block-2 geometry, stamped by the
   * shell when objects from several zones are merged into one composite scene.
   *
   * The key is `(zone path, real offset)` — see {@link zoneGeometryKey} — and a
   * composite scene destroys both halves: its `path` is every loaded zone path
   * joined together and its offsets are slot-shifted to stay unique. Without
   * this the renderer would ask `level_zone_geometry` for a zone that does not
   * exist and every impostor and decal on a multi-zone load would stay a
   * wireframe box.
   */
  geometryKey?: string;
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
  /** Real block sizes, block 0 first — the prefix of `sizes` that is trustworthy. */
  blocks?: number[];
  /** Scene objects carrying a mesh in block 2 (impostors, decals). */
  blockGeometryCount?: number;
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

/**
 * Loads an RCMS mesh buffer for one cache key.
 *
 * A key is either a model asset id (`"0x…"`, served by `level_model_mesh`) or a
 * zone-geometry key built by {@link zoneGeometryKey} (served by
 * `level_zone_geometry`). Folding both into one keyspace means the viewport
 * keeps a single progressive queue and a single geometry cache, and the shell
 * keeps passing one loader down — see `makeMeshLoader` in `api.ts`.
 */
export type MeshLoader = (key: string) => Promise<ArrayBuffer>;

const ZONE_GEOMETRY_PREFIX = "zone:";

/**
 * Cache key for the block-2 mesh of one scene object: `zone:<zonePath>#<offset>`.
 *
 * Zone asset paths are lowercase forward-slash paths and never contain `#`, so
 * splitting on the last `#` round-trips exactly.
 */
export function zoneGeometryKey(zonePath: string, offset: number): string {
  return `${ZONE_GEOMETRY_PREFIX}${zonePath}#${offset}`;
}

/** Inverse of {@link zoneGeometryKey}; `null` for a plain model id. */
export function parseZoneGeometryKey(key: string): { zonePath: string; offset: number } | null {
  if (!key.startsWith(ZONE_GEOMETRY_PREFIX)) return null;
  const hash = key.lastIndexOf("#");
  if (hash < ZONE_GEOMETRY_PREFIX.length) return null;
  const offset = Number(key.slice(hash + 1));
  if (!Number.isInteger(offset) || offset < 0) return null;
  return { zonePath: key.slice(ZONE_GEOMETRY_PREFIX.length, hash), offset };
}

export type GizmoMode = "translate" | "rotate" | "scale";

// ---------------------------------------------------------------------------
// level_model_materials / level_texture
// ---------------------------------------------------------------------------

/**
 * One texture a material slot binds. Mirrors the Rust `TextureRef`.
 *
 * `textureId` is the cache key for the whole viewport: a planet reuses a few
 * hundred textures across tens of thousands of submeshes, so everything
 * downstream is keyed on this string and never on the path.
 */
export interface TextureRefDto {
  /**
   * The sampler's semantic: `albedo` | `normal` | `roughness` | `mask` |
   * `height` | `ao` | `id` | `emissive` | `detail` | `other`.
   */
  slot: string;
  /** `"0x…"` texture asset id; `"0x0"` when the path is empty. */
  textureId: string;
  /** Lowercase forward-slash asset path; `""` when unknown. */
  texturePath: string;
  /** Native size (first streamed mip when there is one); 0 when unresolved. */
  width: number;
  height: number;
  /** `DXGI_FORMAT` name, `""` when unresolved. */
  format: string;
  /** The slot name came from the material's template, not from the filename. */
  named: boolean;
  /** Binds one of the shipped placeholder textures. */
  placeholder: boolean;
  srgb: boolean;
  /** The asset was found in the TOC and its header parsed. */
  resolved: boolean;
}

/**
 * One entry of a model's material table. Mirrors the Rust `MaterialSlot`.
 *
 * `index` is exactly the `materialIndex` an RCMS submesh carries, so the
 * viewport turns submeshes into geometry groups and indexes this array with
 * the group's material index and nothing else.
 */
export interface ModelMaterialSlotDto {
  index: number;
  /** `"0x…"` `.material` asset id; `"0x0"` when the model names none. */
  materialId: string;
  /** Lowercase forward-slash path; `""` when unknown. */
  materialPath: string;
  mappingName: string;
  /** The `.materialgraph` this material instances; `""` when it names none. */
  template: string;
  resolved: boolean;
  /**
   * The viewport's fallback signal: a shipped placeholder material, a material
   * missing from the archives, or one whose every texture is a placeholder.
   * These keep the flat per-model hue instead of papering a pink debug grid
   * over the level.
   */
  placeholder: boolean;
  textures: TextureRefDto[];
  /**
   * Present only when the material instances the `gbl_rgb_blend` family, in
   * which case ONE albedo out of `textures` is not what the surface shows.
   * Additive: a caller that ignores this behaves exactly as it did before.
   */
  blend?: BlendRecipeDto | null;
}

/** One layer of an RGB-splat blend. Mirrors the Rust `BlendLayer`. */
export interface BlendLayerDto {
  /** `base` | `r` | `g` | `b` — which splat channel fades this layer in. */
  channel: string;
  textureId: string;
  texturePath: string;
  /** False when the layer has no sampler of its own and reads the base's map. */
  ownTexture: boolean;
  /** Multiplies UV0 for this layer. */
  uvScale: [number, number];
  /** `[hue, saturation, luminance]`, each `0..1` with **0.5 neutral**. */
  hsl: [number, number, number];
  /** `"measured"` | `"assumed"` — how the triple was tied to this layer. */
  hslSource: string;
  scale: number;
}

/** Mirrors the Rust `BlendRecipe`. See `model_materials.rs` for the derivation. */
export interface BlendRecipeDto {
  /** `"rgb_blend"` today. */
  kind: string;
  template: string;
  maskTextureId: string;
  maskTexturePath: string;
  /** base, r, g, b — but a variant may declare fewer, so read `channel`. */
  layers: BlendLayerDto[];
  normalTextureId: string;
  normalTexturePath: string;
  miscTextureId: string;
  miscTexturePath: string;
  params: Record<string, number[]>;
}

export interface ModelMaterialsDto {
  /** `"0x…"`, echoing the request. */
  modelId: string;
  slots: ModelMaterialSlotDto[];
}

/** Filename suffix -> "this is a base-colour map". */
const COLOUR_SUFFIXES = new Set(["c", "col", "color", "colour", "albedo", "basecolor", "d", "diff"]);

/**
 * Filename suffix -> "this is definitely NOT a base-colour map", whatever the
 * material's sampler table called it. See {@link albedoOf}.
 */
const NON_COLOUR_SUFFIXES = new Set([
  "id", "ids", "n", "nrm", "norm", "normal", "m", "mp", "mask", "masks",
  "h", "height", "ao", "occl", "g", "gloss", "glossiness", "r", "rough",
  "roughness", "e", "emiss", "emissive", "glow", "det", "detail",
]);

function textureSuffix(path: string): string {
  const file = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  const stem = file.endsWith(".texture") ? file.slice(0, -".texture".length) : file;
  const cut = stem.lastIndexOf("_");
  return cut < 0 ? "" : stem.slice(cut + 1);
}

/**
 * The albedo this slot should be drawn with, or null to fall back to the flat
 * per-model hue.
 *
 * Three things make this more than "take the first one".
 *
 * A slot can bind several textures the backend calls `albedo` — terrain and
 * cliff shaders blend two to five colour layers through a mask, and 1036 of
 * savali's 2197 material slots bind five. Only one layer can be drawn without
 * running the real shader graph, so the first usable one is the base layer.
 *
 * Worse, the backend's `albedo` label comes from the *material's own sampler
 * table* when the material declares one, and shipped materials declare
 * ID/mask maps under colour-sounding sampler names. Measured over savali's
 * 2,066 textured slots: 492 (24%) name a non-colour map first, all but two of
 * them a `*_id` ID map, which draws as flat radioactive green over whole
 * cliffs. The filename suffix is the more reliable witness, so a candidate
 * whose name ends in a known non-colour suffix is refused outright, and a
 * candidate whose name ends in a colour suffix is preferred. That recovers a
 * real colour map for 292 of the 492 and drops the other 200 back to the
 * honest flat hue.
 *
 * Finally, a placeholder (27% of shipped materials bind nothing but 2x2 pink)
 * or an unresolved asset is deliberately not drawn — papering a pink debug grid
 * over the level is worse than untextured clay.
 */
/**
 * Sampler labels a colour map may be rescued from when the slot's own
 * `albedo` samplers are all non-colour. `normal` / `height` / `roughness` /
 * `ao` / `id` are excluded: a `*_c` file bound there is being consumed as
 * something other than colour, and drawing it as albedo would be a guess.
 */
const RESCUE_LABELS = new Set(["albedo", "mask", "other", "detail", "emissive"]);

export function albedoOf(slot: ModelMaterialSlotDto): TextureRefDto | null {
  if (slot.placeholder) return null;
  const textures = slot.textures ?? [];
  let fallback: TextureRefDto | null = null;
  for (const t of textures) {
    if (t.slot !== "albedo" || t.placeholder || !t.resolved) continue;
    if (!t.textureId || t.textureId === "0x0") continue;
    const suffix = textureSuffix(t.texturePath ?? "");
    if (NON_COLOUR_SUFFIXES.has(suffix)) continue;
    if (COLOUR_SUFFIXES.has(suffix)) return t;
    // Unknown suffix: usable, but keep looking for a named colour map.
    fallback ??= t;
  }

  // The `gbl_rgb_blend` family — 64% of a Nefarious City art tile's slots —
  // declares its ID mask under the `albedo` sampler and its actual layer colour
  // maps (`textures/sbs/metal/steel_001_c` and friends) under `mask`. Taking
  // the sampler name at face value there leaves two thirds of the level flat.
  // A `*_c` filename under a colour-consuming sampler is the base layer, which
  // is the one layer drawable without running the blend graph. Measured on
  // nefarious_city tile_d17_art: 337/1198 slots textured before, 1089/1198
  // after, for 91 distinct textures instead of 42.
  for (const t of textures) {
    if (!RESCUE_LABELS.has(t.slot) || t.placeholder || !t.resolved) continue;
    if (!t.textureId || t.textureId === "0x0") continue;
    if (COLOUR_SUFFIXES.has(textureSuffix(t.texturePath ?? ""))) return t;
  }
  return fallback;
}

// ---------------------------------------------------------------------------
// The `gbl_rgb_blend` family, reduced to what the viewport can evaluate
// ---------------------------------------------------------------------------

/** One layer of a {@link BlendPlan}. `textureId` is never empty. */
export interface BlendPlanLayer {
  textureId: string;
  uvScale: [number, number];
  hsl: [number, number, number];
}

/**
 * A blend the viewport can actually draw: one splat map and exactly four
 * layers, in the order base, r, g, b.
 *
 * The backend's recipe is looser than this — a three-layer variant of the
 * template ships one fewer layer, and a layer with no sampler of its own is
 * already resolved to the base map. `enabled` carries the first of those: it
 * is false for a channel the template never declared, and that channel's weight
 * is forced to zero rather than being drawn as a duplicate of the base.
 */
export interface BlendPlan {
  maskTextureId: string;
  /** Exactly four: base, r, g, b. */
  layers: BlendPlanLayer[];
  /** r, g, b — false where the template declares no such layer. */
  enabled: [boolean, boolean, boolean];
  /** Distinct texture ids this plan needs, mask included. For accounting. */
  textureIds: string[];
}

/** 0.5 is the shipped default of all twelve HSL constants, i.e. "no shift". */
function neutralHsl(h: readonly number[] | undefined): boolean {
  return (
    !h ||
    h.length < 3 ||
    (Math.abs(h[0] - 0.5) < 2e-3 && Math.abs(h[1] - 0.5) < 2e-3 && Math.abs(h[2] - 0.5) < 2e-3)
  );
}

function usableId(id: string | undefined | null): string | null {
  return id && id !== "0x0" && id !== "0x" ? id : null;
}

const BLEND_CHANNELS = ["base", "r", "g", "b"] as const;

/**
 * The blend this slot should be drawn with, or null to fall back to the single
 * albedo {@link albedoOf} picks.
 *
 * Null is returned — deliberately — in every case where the blend would draw
 * the same pixels as the cheap path or worse:
 *
 *   - no recipe, or a `kind` this build does not know;
 *   - a placeholder slot (the flat hue is the honest answer there);
 *   - no usable splat map, or no usable base layer. The splat map is the whole
 *     mechanism: without it there is nothing to blend *by*, and the base layer
 *     is what the surface falls back to everywhere the mask is dark;
 *   - a recipe that reduces to one texture drawn flat — every declared layer
 *     reading the base's map with the same (or a neutral) HSL shift. Roughly a
 *     fifth of the family's shipped materials are this, and drawing them
 *     through the blend shader would cost four extra samplers per fragment to
 *     reproduce `map` exactly.
 */
export function blendPlanOf(slot: ModelMaterialSlotDto): BlendPlan | null {
  const recipe = slot.blend;
  if (!recipe || recipe.kind !== "rgb_blend" || slot.placeholder) return null;

  const mask = usableId(recipe.maskTextureId);
  if (!mask) return null;

  const byChannel = new Map((recipe.layers ?? []).map((l) => [l.channel, l]));
  const base = byChannel.get("base");
  const baseId = usableId(base?.textureId);
  if (!base || !baseId) return null;

  const layers: BlendPlanLayer[] = [];
  const enabled: [boolean, boolean, boolean] = [false, false, false];
  for (let i = 0; i < 4; i++) {
    const l = byChannel.get(BLEND_CHANNELS[i]);
    const id = usableId(l?.textureId) ?? baseId;
    layers.push({
      textureId: id,
      uvScale: l && l.uvScale && l.uvScale.length >= 2 ? [l.uvScale[0], l.uvScale[1]] : [1, 1],
      hsl: l && l.hsl && l.hsl.length >= 3 ? [l.hsl[0], l.hsl[1], l.hsl[2]] : [0.5, 0.5, 0.5],
    });
    if (i > 0) enabled[i - 1] = Boolean(l);
  }

  // Would the blend show anything the base map alone does not?
  let meaningful = !neutralHsl(layers[0].hsl);
  for (let i = 1; i < 4 && !meaningful; i++) {
    if (!enabled[i - 1]) continue;
    const l = layers[i];
    meaningful =
      l.textureId !== baseId ||
      !neutralHsl(l.hsl) ||
      Math.abs(l.uvScale[0] - layers[0].uvScale[0]) > 1e-3 ||
      Math.abs(l.uvScale[1] - layers[0].uvScale[1]) > 1e-3;
  }
  if (!meaningful) return null;

  const ids = new Set<string>([mask]);
  for (let i = 0; i < 4; i++) if (i === 0 || enabled[i - 1]) ids.add(layers[i].textureId);
  return { maskTextureId: mask, layers, enabled, textureIds: [...ids] };
}

/** Resolves a model's material table. Rejects are treated as "no materials". */
export type MaterialLoader = (modelId: string) => Promise<ModelMaterialsDto | null>;

/**
 * Fetches the raw texture payload for one texture id, capped to `maxSize` on
 * the longest edge. See `decodeTexturePayload` for the accepted containers.
 */
export type TextureLoader = (textureId: string, maxSize: number) => Promise<ArrayBuffer>;

/** The pair of loaders the viewport needs in order to draw textured. */
export interface TextureSource {
  materials: MaterialLoader;
  texture: TextureLoader;
}
