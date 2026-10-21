/**
 * LevelRenderer — the imperative three.js side of the level viewport.
 *
 * Owns the WebGL2 context, the instanced scene graph, picking, the transform
 * gizmo and the render loop. The React component is a thin shell around this so
 * that nothing per-frame ever goes through React state.
 *
 * Scene layout
 *   modelGroup    one THREE.InstancedMesh per unique model asset (GPU instancing
 *                 is mandatory: a tile can hold 5k instances, a planet ~180k)
 *   blockGeoGroup one single-instance mesh per scene object that owns geometry
 *                 inside the zone's raw block 2 — kind 6 impostor shells and
 *                 kind 3 projected decal sheets. Neither has a `.model` asset,
 *                 so these triangles exist nowhere else: measured per zone,
 *                 tile_c17_art (savali) 91 objects / 346,820 triangles,
 *                 tile_e19_art (nefarious) 218 / 204,872, tile_m37_art
 *                 (megalopolis) 205 / 26,955, tile_n19_art (sargasso) 8 / 2,107,
 *                 and a megalopolis impostor zone is 100% impostor. They used
 *                 to draw as empty wireframe boxes.
 *   entityGroup   one InstancedMesh of wireframe boxes per entity kind, plus a
 *                 screen-sized Points cloud so markers stay visible at range
 *   helpers       grid, axes, selection outline boxes, gizmo
 */

import * as THREE from "three";
import { TransformControls } from "three/examples/jsm/controls/TransformControls.js";
import { CameraRig } from "./cameraRig";
import { MeshQueue } from "./meshCache";
import { isFiniteMatrix, zoneToMatrix4 } from "./matrix";
import {
  BLOCK_GEO_TINTS,
  BLOCK_GEO_NEUTRAL,
  createBlockGeoMaterial,
  createLightRig,
  depthBiasFor,
  createModelMaterial,
  isBlendMaterial,
  updateBlendMaterial,
  usesTextures,
  type BlendInput,
  type BlockGeoKind,
  type ShadingMode,
} from "./materials";
import {
  TEXTURE_MAX_SIZE,
  TextureQueue,
  setTextureContext,
  textureCacheStats,
} from "./textureCache";
import {
  KIND_DECAL,
  KIND_IMPOSTOR,
  KIND_MODEL,
  albedoOf,
  blendPlanOf,
  zoneGeometryKey,
  type BlendPlan,
  type GizmoMode,
  type MeshLoader,
  type ModelMaterialsDto,
  type SceneObjectDto,
  type TextureSource,
  type ZoneScene,
} from "../types";

// ---------------------------------------------------------------------------

export interface HudState {
  fps: number;
  drawCalls: number;
  triangles: number;
  instances: number;
  entities: number;
  models: number;
  loading: number;
  cameraPos: [number, number, number];
  hovered: string | null;
  selectedCount: number;
  selectedName: string | null;
  /** Instances whose 4x4 has a negative-determinant basis (mirrored art). */
  mirrored: number;
  /** Dev builds only: decoded meshes whose winding disagrees with their normals. */
  windingSuspect: number;
  /** Scene objects carrying a mesh inside the zone's raw block 2. */
  blockGeo: number;
  /** Of those, how many have their real geometry on screen (rest are boxes). */
  blockGeoLoaded: number;
  /** Triangles those meshes contribute. */
  blockGeoTriangles: number;
  /** Distinct albedo textures resident in the shared cache. */
  textures: number;
  /** Approximate GPU bytes those textures occupy. */
  textureBytes: number;
  /** Texture fetches still outstanding. */
  textureLoading: number;
  /** Textures refused because the VRAM budget was already spent. */
  textureSkipped: number;
  /** Model batches drawing at least one textured submesh. */
  texturedModels: number;
  /** Submesh materials evaluating the `gbl_rgb_blend` RGB-splat blend. */
  blendedSubmeshes: number;
  /** Metres to the surface the camera is pointed at; null = nothing out there. */
  surfaceDist: number | null;
  /** Metres per second the fly keys move at right now (what the probe buys). */
  moveSpeed: number;
  /** Mean cost of one surface probe, milliseconds. */
  probeMs: number;
}

export interface LevelRendererOptions {
  container: HTMLElement;
  onSelect: (offsets: number[], additive: boolean) => void;
  onTransform?: (offset: number, matrix: number[]) => void;
  onHud: (hud: HudState) => void;
  onBoxSelect?: (rect: { x: number; y: number; w: number; h: number } | null) => void;
}

/**
 * `model`    instanced `.model` asset geometry, one batch per asset.
 * `entity`   wireframe marker boxes, one batch per scene-object kind.
 * `blockGeo` one scene object's own mesh out of the zone's raw block 2. Each
 *            such mesh is unique to its object, so these are single-instance
 *            batches — they stay `InstancedMesh` purely so that picking,
 *            per-instance tinting, the gizmo and box-select need no special
 *            case, and a one-instance InstancedMesh is one draw call either way.
 */
type BatchKind = "model" | "entity" | "blockGeo";

interface Batch {
  key: string;
  mesh: THREE.InstancedMesh;
  objects: SceneObjectDto[];
  kind: BatchKind;
  /** `blockGeo` only. */
  geoKind?: BlockGeoKind;
  /** `blockGeo` only: real geometry has replaced the placeholder marker box. */
  resolved: boolean;
  /** `model` only: the material table, once `level_model_materials` answered. */
  materials?: ModelMaterialsDto | null;
  /**
   * `model` only: what each entry of the material ARRAY samples, parallel to
   * `mesh.material` when that is an array (and a one-element array when it is
   * a single material). Lets a texture arriving later patch exactly the slots
   * that asked for it instead of rebuilding.
   */
  slotTextures?: SlotTextures[];
  /** `model` only: how many of its materials evaluate an RGB-splat blend. */
  blended?: number;
  /**
   * Per-instance world bounding spheres as [cx, cy, cz, r] quads, built lazily
   * for the camera's surface probe and dropped whenever the geometry or an
   * instance matrix changes. See `surfaceProbe`.
   */
  probeBounds?: Float32Array | null;
}

/**
 * What one material of a batch samples.
 *
 * `baseId` is the ONE map the material draws flat right now: the single albedo
 * for a plain slot, the base layer once a blended slot has been promoted, and
 * `albedoOf`'s pick in between — a blend whose base layer has not arrived (or
 * will not decode) keeps drawing exactly what this viewport drew before the
 * blend existed.
 */
interface SlotTextures {
  baseId: string | null;
  /** Present when this slot is a `gbl_rgb_blend` surface. */
  plan?: BlendPlan;
}

/** Does this slot sample the given texture id in any role? */
function usesTexture(use: SlotTextures, id: string): boolean {
  if (use.baseId === id) return true;
  if (!use.plan) return false;
  return use.plan.textureIds.includes(id);
}

/**
 * Submesh tables, keyed the same way meshes are.
 *
 * `decodeRcms` throws the submesh table away and `rcms.ts` is not this change's
 * to edit, so the game is wrapped (see `setScene`) and the table read off the
 * raw buffer here. Module level, because `MeshQueue`'s geometry cache can hand
 * back a geometry whose buffer this process already consumed and will not fetch
 * again. 16 bytes per submesh, so a whole planet's tables are well under a
 * megabyte. TODO: fold this into `rcmsToGeometry` (as `geometry.groups`) once
 * that file is free to change.
 */
const submeshTables = new Map<string, { firstIndex: number; indexCount: number; materialIndex: number }[]>();

/** Read only the header and submesh table of an RCMS buffer. */
function readRcmsSubmeshes(
  buf: ArrayBuffer,
): { firstIndex: number; indexCount: number; materialIndex: number }[] | null {
  if (!buf || buf.byteLength < 48) return null;
  const dv = new DataView(buf);
  if (dv.getUint32(0, true) !== 0x534d4352 || dv.getUint32(4, true) !== 1) return null;
  const vertexCount = dv.getUint32(8, true);
  const indexCount = dv.getUint32(12, true);
  const submeshCount = dv.getUint32(16, true);
  if (!vertexCount || !indexCount || !submeshCount) return null;
  const base = 48 + vertexCount * 8 * 4 + indexCount * 4;
  if (base + submeshCount * 16 > buf.byteLength) return null;
  const out = [];
  for (let i = 0; i < submeshCount; i++) {
    const b = base + i * 16;
    out.push({
      firstIndex: dv.getUint32(b, true),
      indexCount: dv.getUint32(b + 4, true),
      materialIndex: dv.getUint32(b + 8, true),
    });
  }
  return out;
}

/**
 * Turn the submesh table into geometry GROUPS.
 *
 * One RCMS mesh is one BufferGeometry whose submeshes are index ranges, so the
 * textured path draws it as one InstancedMesh with an array of materials — one
 * per group — rather than splitting the model into several meshes. Splitting
 * would multiply the instance buffers (a tile holds thousands of instances per
 * model) for nothing: the draw-call count is identical either way, because
 * three issues one call per group.
 *
 * Groups are idempotent and belong to the geometry, which is shared through the
 * mesh cache, so this runs at most once per model.
 */
function applyGroups(geometry: THREE.BufferGeometry, key: string): void {
  if (geometry.groups.length > 0) return;
  const subs = submeshTables.get(key);
  if (!subs || subs.length < 2) return; // a single submesh needs no split
  const idx = geometry.getIndex();
  const total = idx ? idx.count : 0;
  for (const s of subs) {
    if (s.indexCount <= 0 || s.firstIndex + s.indexCount > total) continue;
    geometry.addGroup(s.firstIndex, s.indexCount, s.materialIndex);
  }
}

function disposeMaterial(m: THREE.Material | THREE.Material[]): void {
  if (Array.isArray(m)) for (const x of m) x.dispose();
  else m.dispose();
}

/** True while a batch is drawn as a scale-hint wireframe box rather than a mesh. */
function isMarkerBox(b: Batch): boolean {
  return b.kind === "entity" || (b.kind === "blockGeo" && !b.resolved);
}

interface Slot {
  batch: Batch;
  index: number;
  object: SceneObjectDto;
}

/** Colour-coded entity kinds. Kind 0 (model) never reaches the entity group. */
export const KIND_COLORS: Record<number, number> = {
  1: 0xffd166, // light
  2: 0x3fb950, // actor
  3: BLOCK_GEO_TINTS.decal, // decal
  4: 0x39c5cf, // spline
  5: 0xf0883e, // vfx
  6: BLOCK_GEO_TINTS.impostor, // impostor
};
const KIND_FALLBACK = 0x8b949e;

const SELECT_COLOR = new THREE.Color(0x58a6ff);
const HOVER_COLOR = new THREE.Color(0xffffff);

/** Above this instance count the per-move hover raycast is skipped. */
const HOVER_INSTANCE_LIMIT = 90000;

let placeholderGeometry: THREE.BoxGeometry | null = null;
function getPlaceholderGeometry(): THREE.BoxGeometry {
  if (!placeholderGeometry) placeholderGeometry = new THREE.BoxGeometry(1, 1, 1);
  return placeholderGeometry;
}

let entityGeometry: THREE.BoxGeometry | null = null;
function getEntityGeometry(): THREE.BoxGeometry {
  if (!entityGeometry) entityGeometry = new THREE.BoxGeometry(1, 1, 1);
  return entityGeometry;
}

/**
 * The mesh the surface probe borrows to raycast one instance exactly.
 *
 * Its material is double-sided so the probe sees a wall from inside as well as
 * outside, and its matrices are written directly (no auto-update) because the
 * probe hands it a scene-object 4x4 and immediately raycasts.
 */
function makeProbeMesh(): THREE.Mesh {
  const mesh = new THREE.Mesh(new THREE.BufferGeometry(), new THREE.MeshBasicMaterial({ side: THREE.DoubleSide }));
  mesh.matrixAutoUpdate = false;
  mesh.matrixWorldAutoUpdate = false;
  mesh.frustumCulled = false;
  return mesh;
}

/** The wireframe marker box every non-model object starts (and may stay) as. */
function makeMarkerMaterial(): THREE.Material {
  return new THREE.MeshBasicMaterial({ color: 0xffffff, wireframe: true, transparent: true, opacity: 0.9 });
}

/**
 * Does this entry own a mesh inside the zone's raw block 2?
 *
 * `SceneObjectDto.geometry` is set by the backend only for kind 3 / kind 6
 * entries whose span it validated, so this is a free, exact predicate: we ask
 * `level_zone_geometry` for exactly the objects that have something to return,
 * never once per boxy entity. The kind check is belt-and-braces — the backend
 * refuses to decode anything else — and `indexCount` filters spans that would
 * decode to zero triangles.
 */
function hasBlockGeometry(o: SceneObjectDto): boolean {
  if (o.kind !== KIND_DECAL && o.kind !== KIND_IMPOSTOR) return false;
  const g = o.geometry;
  return Boolean(g && g.indexCount >= 3 && g.vertexCount >= 3);
}

/** Stable pastel colour per model id, so instances of one model read as a set. */
function colorForKey(key: string): THREE.Color {
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  const hue = ((h >>> 8) % 360) / 360;
  return new THREE.Color().setHSL(hue, 0.28, 0.62);
}

// ---------------------------------------------------------------------------

export class LevelRenderer {
  private container: HTMLElement;
  private opts: LevelRendererOptions;

  private renderer: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera: THREE.PerspectiveCamera;
  private rig: CameraRig;
  private raycaster = new THREE.Raycaster();
  /** Kept apart from `raycaster` so a camera probe cannot clobber a pick. */
  private probeRaycaster = new THREE.Raycaster();
  /** World point of the last `pickAt` triangle hit; see `pickPointValid`. */
  private pickPoint = new THREE.Vector3();
  /** False when the last pick came from the screen-space entity fallback. */
  private pickPointValid = false;
  /** Scratch for the surface probe; see `surfaceProbe`. */
  private probeVec = new THREE.Vector3();
  private probeCandidates: { batch: Batch; index: number; near: number }[] = [];
  private probeHits: THREE.Intersection[] = [];
  private probeMatrix = new THREE.Matrix4();
  /** Stand-in for one instance during the probe's exact pass. */
  private probeMesh = makeProbeMesh();

  private modelGroup = new THREE.Group();
  private blockGeoGroup = new THREE.Group();
  private entityGroup = new THREE.Group();
  private helperGroup = new THREE.Group();

  private grid: THREE.GridHelper | null = null;
  private axes: THREE.AxesHelper | null = null;
  private outline: THREE.LineSegments;

  private gizmo: TransformControls;
  private gizmoProxy = new THREE.Object3D();
  private gizmoMode: GizmoMode | null = "translate";

  private batches: Batch[] = [];
  private batchByKey = new Map<string, Batch>();
  private batchByMesh = new Map<THREE.Object3D, Batch>();
  private slots = new Map<number, Slot>();
  private matrices = new Map<number, THREE.Matrix4>();
  private meshQueue: MeshQueue | null = null;
  private sceneBounds = new THREE.Box3();
  private currentPath: string | null = null;

  /** Materials + textures for the loaded scene; null before the backend lands. */
  private textureSource: TextureSource | null = null;
  private textureQueue: TextureQueue | null = null;
  /** Resolved textures by asset id (`null` = asked for, came back unusable). */
  private textures = new Map<string, THREE.Texture | null>();
  /** Which model batches want each texture id, so an arrival patches only those. */
  private textureUsers = new Map<string, Set<Batch>>();
  /** Model ids whose material table has been requested this scene. */
  private materialsRequested = new Set<string>();
  private materialsInFlight = 0;
  private materialsPending: string[] = [];

  private selection: number[] = [];
  private selectionSet = new Set<number>();
  private hovered: number | null = null;

  private showGeometry = true;
  private showEntities = true;
  private wireframe = false;
  /**
   * Textured is the default: it is what the level actually looks like, and the
   * flat-hue `lit` mode stays one key away because reading structure (which
   * prop is which, where a model repeats) is genuinely easier without albedo.
   */
  private shading: ShadingMode = "textured";

  private modelInstanceCount = 0;
  private entityInstanceCount = 0;
  private uniqueModels = 0;
  private mirroredInstanceCount = 0;
  private suspectMeshCount = 0;
  private blockGeoCount = 0;
  private blockGeoLoaded = 0;
  private blockGeoTriangles = 0;

  private needsRender = true;
  private running = true;
  private rafId = 0;
  private lastTime = performance.now();
  private frameTimes: number[] = [];
  private lastHud = 0;
  private lastHover = 0;
  /** Camera position at the previous HUD tick; see `emitHud`. */
  private lastHudCamera = new THREE.Vector3(NaN, NaN, NaN);

  private resizeObserver: ResizeObserver;

  // pointer state
  private dragButton = -1;
  private dragMoved = false;
  private dragStart = { x: 0, y: 0 };
  private lastPointer = { x: 0, y: 0 };
  private boxSelecting = false;
  private gizmoDragging = false;
  private dragSnapshot: { start: THREE.Matrix4[]; offsets: number[]; proxyInv: THREE.Matrix4 } | null = null;

  constructor(opts: LevelRendererOptions) {
    this.opts = opts;
    this.container = opts.container;

    this.renderer = new THREE.WebGLRenderer({
      antialias: true,
      logarithmicDepthBuffer: true,
      powerPreference: "high-performance",
    });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    this.renderer.setClearColor(0x0f1117, 1);
    // Untextured clay under a bright key blows out fast; ACES keeps the
    // highlights inside the range where shading is still readable.
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    // Must agree with `setShadingMode`, which raises the exposure for the
    // textured mode. The viewport BOOTS textured, so hard-coding the clay
    // exposure here left the level 36% darker (measured: mean frame value
    // 35.1 against 47.9 on one nefarious tile) until the first time anyone
    // pressed N — a bug the eye reads as "the albedo is muddy".
    this.renderer.toneMappingExposure = usesTextures(this.shading) ? 1.55 : 1.05;
    this.renderer.domElement.style.display = "block";
    this.renderer.domElement.style.width = "100%";
    this.renderer.domElement.style.height = "100%";
    this.renderer.domElement.style.outline = "none";
    this.container.appendChild(this.renderer.domElement);

    // Which compressed formats this context can take, and the anisotropy
    // ceiling. Level art is mostly ground and walls seen at a glancing angle,
    // where isotropic filtering turns a texture to mush a few metres out.
    setTextureContext(this.renderer);

    // Tiles are hundreds of metres across; a log depth buffer keeps 0.1..20000
    // free of z-fighting.
    this.camera = new THREE.PerspectiveCamera(55, 1, 0.1, 20000);
    this.rig = new CameraRig(this.camera);
    // Every camera rate (fly speed, wheel step, pan) scales with how far the
    // geometry in front of the camera actually is - see `cameraRig.ts`.
    this.rig.setSurfaceProbe(this.surfaceProbe);

    this.scene.add(this.modelGroup, this.blockGeoGroup, this.entityGroup, this.helperGroup);

    this.scene.add(...createLightRig());

    this.outline = new THREE.LineSegments(
      new THREE.BufferGeometry(),
      new THREE.LineBasicMaterial({ color: 0x58a6ff, depthTest: false, transparent: true, opacity: 0.95 }),
    );
    this.outline.renderOrder = 999;
    this.outline.frustumCulled = false;
    this.helperGroup.add(this.outline);

    this.gizmo = new TransformControls(this.camera, this.renderer.domElement);
    this.gizmo.setSize(0.85);
    this.helperGroup.add(this.gizmo.getHelper());
    this.scene.add(this.gizmoProxy);
    this.bindGizmo();

    this.bindPointer();
    this.bindKeyboard();

    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(this.container);
    this.resize();

    this.buildHelpers(new THREE.Box3(new THREE.Vector3(-100, 0, -100), new THREE.Vector3(100, 0, 100)));
    this.rig.frameBoxDefaultAngle(new THREE.Box3(new THREE.Vector3(-40, -10, -40), new THREE.Vector3(40, 20, 40)));

    this.loop();
  }

  // -- public API -----------------------------------------------------------

  /** Load (or clear) a zone. Meshes stream in behind placeholder boxes. */
  setScene(scene: ZoneScene | null, meshLoader: MeshLoader, textureSource?: TextureSource | null): void {
    const previousPath = this.currentPath;
    this.clearScene();
    this.textureSource = textureSource ?? null;
    this.currentPath = scene?.path ?? null;
    if (!scene) {
      this.needsRender = true;
      return;
    }
    // Re-supplying the same zone (e.g. the shell rebuilt its state object after
    // an edit) must not yank the camera back to the default framing.
    const keepCamera = previousPath === scene.path;

    const byModel = new Map<string, SceneObjectDto[]>();
    const byKind = new Map<number, SceneObjectDto[]>();
    const blockGeo: SceneObjectDto[] = [];
    const bounds = new THREE.Box3();
    const p = new THREE.Vector3();

    for (const o of scene.objects) {
      if (!isFiniteMatrix(o.matrix)) continue;
      p.set(o.matrix[12], o.matrix[13], o.matrix[14]);
      bounds.expandByPoint(p);
      if (o.kind === KIND_MODEL && o.modelId && o.modelId !== "0x0") {
        let list = byModel.get(o.modelId);
        if (!list) byModel.set(o.modelId, (list = []));
        list.push(o);
      } else if (hasBlockGeometry(o)) {
        // The entry itself tells us it owns a mesh, so we never spend an IPC
        // round trip on a decal or impostor that has none — and every other
        // kind keeps its marker box without asking the backend at all.
        blockGeo.push(o);
      } else if (o.kind !== KIND_MODEL) {
        let list = byKind.get(o.kind);
        if (!list) byKind.set(o.kind, (list = []));
        list.push(o);
      }
    }
    this.blockGeoCount = blockGeo.length;

    if (bounds.isEmpty()) bounds.set(new THREE.Vector3(-50, 0, -50), new THREE.Vector3(50, 0, 50));
    // Give flat zones some vertical room so framing does not degenerate.
    const size = bounds.getSize(new THREE.Vector3());
    if (size.y < 1) bounds.expandByVector(new THREE.Vector3(0, 5, 0));
    this.sceneBounds.copy(bounds);

    // Model batches, biggest first so they stream in first.
    const order = scene.models.map((m) => m.id).filter((id) => byModel.has(id));
    const ordered = new Set(order);
    for (const id of byModel.keys()) if (!ordered.has(id)) order.push(id);

    const m4 = new THREE.Matrix4();
    for (const modelId of order) {
      const objects = byModel.get(modelId)!;
      const mesh = new THREE.InstancedMesh(
        getPlaceholderGeometry(),
        createModelMaterial(this.shading, this.wireframe, { tint: colorForKey(modelId) }),
        objects.length,
      );
      mesh.name = modelId;
      mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
      const base = this.baseInstanceColor(modelId);
      for (let i = 0; i < objects.length; i++) {
        const o = objects[i];
        zoneToMatrix4(o.matrix, m4);
        mesh.setMatrixAt(i, m4);
        mesh.setColorAt(i, base);
        this.matrices.set(o.offset, m4.clone());
        if (determinant3(o.matrix) < 0) this.mirroredInstanceCount++;
      }
      mesh.instanceMatrix.needsUpdate = true;
      if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
      mesh.computeBoundingSphere();

      this.registerBatch({ key: modelId, mesh, objects, kind: "model", resolved: true }, this.modelGroup);
      this.modelInstanceCount += objects.length;
    }
    this.uniqueModels = order.length;

    // Block-2 geometry: one single-instance batch per object, drawn as the same
    // wireframe marker box it used to be until its mesh arrives (and left that
    // way for good if the decode comes back empty).
    // A composite scene stamps the real `(zone path, offset)` key on the object
    // itself, because neither half survives the merge; a single-zone scene has
    // both to hand.
    const geoKeyOf = (o: SceneObjectDto) => o.geometryKey ?? zoneGeometryKey(scene.path, o.offset);

    for (const o of blockGeo) {
      const geoKind: BlockGeoKind = o.kind === KIND_IMPOSTOR ? "impostor" : "decal";
      const mesh = new THREE.InstancedMesh(getEntityGeometry(), makeMarkerMaterial(), 1);
      mesh.name = `${geoKind}@${o.offset}`;
      mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
      zoneToMatrix4(o.matrix, m4);
      this.matrices.set(o.offset, m4.clone());
      mesh.setMatrixAt(0, this.entityMarkerMatrix(o, m4.clone()));
      mesh.setColorAt(0, new THREE.Color(BLOCK_GEO_TINTS[geoKind]));
      mesh.instanceMatrix.needsUpdate = true;
      if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
      mesh.computeBoundingSphere();
      if (determinant3(o.matrix) < 0) this.mirroredInstanceCount++;

      this.registerBatch(
        { key: geoKeyOf(o), mesh, objects: [o], kind: "blockGeo", geoKind, resolved: false },
        this.blockGeoGroup,
      );
    }

    // Entity batches — wireframe boxes, one InstancedMesh per kind.
    const pos: number[] = [];
    const col: number[] = [];
    for (const [kind, objects] of byKind) {
      const color = new THREE.Color(KIND_COLORS[kind] ?? KIND_FALLBACK);
      const mesh = new THREE.InstancedMesh(getEntityGeometry(), makeMarkerMaterial(), objects.length);
      mesh.name = `kind${kind}`;
      mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
      for (let i = 0; i < objects.length; i++) {
        const o = objects[i];
        zoneToMatrix4(o.matrix, m4);
        this.matrices.set(o.offset, m4.clone());
        mesh.setMatrixAt(i, this.entityMarkerMatrix(o, m4.clone()));
        mesh.setColorAt(i, color);
        pos.push(o.matrix[12], o.matrix[13], o.matrix[14]);
        col.push(color.r, color.g, color.b);
      }
      mesh.instanceMatrix.needsUpdate = true;
      if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
      mesh.computeBoundingSphere();

      this.registerBatch({ key: `kind${kind}`, mesh, objects, kind: "entity", resolved: true }, this.entityGroup);
      this.entityInstanceCount += objects.length;
    }

    if (pos.length) {
      const pg = new THREE.BufferGeometry();
      pg.setAttribute("position", new THREE.Float32BufferAttribute(pos, 3));
      pg.setAttribute("color", new THREE.Float32BufferAttribute(col, 3));
      const points = new THREE.Points(
        pg,
        new THREE.PointsMaterial({ size: 6, sizeAttenuation: false, vertexColors: true, depthTest: true }),
      );
      points.name = "entityDots";
      points.raycast = () => {}; // picking goes through the boxes / screen-space test
      this.entityGroup.add(points);
    }

    this.applyGroupVisibility();

    this.buildHelpers(bounds);
    if (!keepCamera) this.rig.frameBoxDefaultAngle(bounds);

    // One queue over one cache for both mesh sources — models first (they are
    // sorted by instance count, so the visually dominant art lands first), then
    // the block-2 meshes, which are one object each and therefore cheaper to
    // wait for.
    //
    // The game is wrapped so the RCMS submesh table is kept as the buffer
    // goes past: `decodeRcms` discards it, and it is what turns submeshes into
    // geometry groups (see `submeshTables`).
    const withSubmeshes: MeshLoader = async (key) => {
      const buf = await meshLoader(key);
      if (!submeshTables.has(key)) {
        const table = readRcmsSubmeshes(buf);
        if (table) submeshTables.set(key, table);
      }
      return buf;
    };
    this.meshQueue = new MeshQueue(withSubmeshes, (key, geom) => this.onGeometry(key, geom), 4);
    for (const modelId of order) this.meshQueue.request(modelId);
    for (const o of blockGeo) this.meshQueue.request(geoKeyOf(o));

    if (this.textureSource) {
      this.textureQueue = new TextureQueue(
        (id) => this.textureSource!.texture(id, TEXTURE_MAX_SIZE),
        (id, tex) => this.onTexture(id, tex),
        6,
      );
      if (usesTextures(this.shading)) this.requestMaterials(order);
    }

    this.setSelection([]);
    this.needsRender = true;
  }

  private registerBatch(batch: Batch, group: THREE.Group): void {
    for (let i = 0; i < batch.objects.length; i++) {
      this.slots.set(batch.objects[i].offset, { batch, index: i, object: batch.objects[i] });
    }
    this.batches.push(batch);
    this.batchByKey.set(batch.key, batch);
    this.batchByMesh.set(batch.mesh, batch);
    group.add(batch.mesh);
  }

  /** Is this batch's group currently drawn? Picking must agree with the eye. */
  private batchVisible(b: Batch): boolean {
    return b.kind === "entity" ? this.showEntities : this.showGeometry;
  }

  /**
   * Block-2 geometry follows `showGeometry`, not `showEntities`.
   *
   * These are real triangle meshes - on savali's `tile_c17_art` they are 346,820
   * of them, and one megalopolis impostor zone is 100% block-2 geometry - so a
   * "hide geometry" that left a skyline standing would be a lie, and "entities
   * only" is asked for precisely when someone wants an unobstructed view of the
   * markers. The placeholder marker box a block-2 object wears before its mesh
   * arrives follows the same toggle, so an object never jumps between groups
   * mid-load. Kinds 3 and 6 that own no block-2 span stay ordinary entities.
   */
  private applyGroupVisibility(): void {
    this.modelGroup.visible = this.showGeometry;
    this.blockGeoGroup.visible = this.showGeometry;
    this.entityGroup.visible = this.showEntities;
  }

  setSelection(offsets: number[]): void {
    const next = new Set(offsets);
    const changed = new Set<number>();
    for (const o of this.selectionSet) if (!next.has(o)) changed.add(o);
    for (const o of next) if (!this.selectionSet.has(o)) changed.add(o);

    this.selection = offsets.slice();
    this.selectionSet = next;
    for (const off of changed) this.refreshColor(off);

    this.updateOutline();
    this.updateGizmoAttachment();
    this.needsRender = true;
  }

  setVisibility(v: { showGeometry: boolean; showEntities: boolean; wireframe: boolean }): void {
    this.showGeometry = v.showGeometry;
    this.showEntities = v.showEntities;
    if (this.wireframe !== v.wireframe) {
      this.wireframe = v.wireframe;
      for (const b of this.batches) {
        // A marker box is already a wireframe; only real meshes have a mode.
        if (isMarkerBox(b)) continue;
        const m = b.mesh.material;
        for (const one of Array.isArray(m) ? m : [m]) {
          (one as THREE.MeshPhongMaterial).wireframe = this.wireframe;
        }
      }
    }
    this.applyGroupVisibility();
    this.updateGizmoAttachment();
    this.needsRender = true;
  }

  /** Swap the model-batch material (textured / lit / normals / facing). */
  setShadingMode(mode: ShadingMode): void {
    if (this.shading === mode) return;
    this.shading = mode;
    for (const b of this.batches) {
      if (isMarkerBox(b)) continue;
      if (b.kind === "blockGeo") {
        const old = b.mesh.material as THREE.Material;
        b.mesh.material = createBlockGeoMaterial(b.geoKind!, this.shading, this.wireframe);
        old.dispose();
      } else {
        this.rebuildBatchMaterials(b);
      }
    }
    // Raw sRGB albedo under this rig reads much darker than the flat clay of
    // `lit` — dark-substance assets (carbon steel, rubber, black paint) fall
    // close to black. Lift the exposure for the textured mode only, so the
    // debug modes keep the contrast they were tuned for.
    this.renderer.toneMappingExposure = usesTextures(mode) ? 1.55 : 1.05;
    // The instance colour means something different in textured mode, and the
    // material tables are only fetched for the mode that needs them.
    this.applyBaseColors();
    if (usesTextures(mode)) {
      this.requestMaterials(this.batches.filter((b) => b.kind === "model").map((b) => b.key));
    }
    this.needsRender = true;
  }

  getShadingMode(): ShadingMode {
    return this.shading;
  }

  setGizmoMode(mode: GizmoMode | null): void {
    this.gizmoMode = mode;
    if (mode) this.gizmo.setMode(mode);
    this.updateGizmoAttachment();
    this.needsRender = true;
  }

  getGizmoMode(): GizmoMode | null {
    return this.gizmoMode;
  }

  setGizmoSpace(space: "world" | "local"): void {
    this.gizmo.setSpace(space);
    this.needsRender = true;
  }

  frameAll(): void {
    if (!this.sceneBounds.isEmpty()) this.rig.frameBox(this.sceneBounds);
    this.needsRender = true;
  }

  frameSelection(): void {
    const box = this.selectionBox();
    if (box) this.rig.frameBox(box, 2.2);
    else this.frameAll();
    this.needsRender = true;
  }

  /** Update objects in place (edit, undo, reload) without rebuilding the scene. */
  applyObjects(objects: SceneObjectDto[]): void {
    const touched = new Set<Batch>();
    for (const o of objects) {
      const slot = this.slots.get(o.offset);
      if (!slot || !isFiniteMatrix(o.matrix)) continue;
      slot.object = o;
      slot.batch.objects[slot.index] = o;
      const m = zoneToMatrix4(o.matrix);
      this.matrices.set(o.offset, m);
      this.writeInstanceMatrix(slot, m);
      touched.add(slot.batch);
    }
    for (const b of touched) b.mesh.computeBoundingSphere();
    this.updateOutline();
    this.updateGizmoAttachment();
    this.needsRender = true;
  }

  dispose(): void {
    this.running = false;
    cancelAnimationFrame(this.rafId);
    this.resizeObserver.disconnect();
    this.unbindPointer();
    this.unbindKeyboard();

    this.gizmo.detach();
    this.gizmo.disconnect();
    this.gizmo.dispose();
    this.helperGroup.remove(this.gizmo.getHelper());

    this.clearScene();
    this.outline.geometry.dispose();
    (this.outline.material as THREE.Material).dispose();
    this.grid?.dispose();
    this.axes?.dispose();

    this.renderer.dispose();
    this.renderer.forceContextLoss();
    if (this.renderer.domElement.parentNode === this.container) {
      this.container.removeChild(this.renderer.domElement);
    }
  }

  // -- scene teardown -------------------------------------------------------

  private clearScene(): void {
    this.rig.invalidateSurface();
    this.meshQueue?.dispose();
    this.meshQueue = null;
    // Releases this scene's hold on every texture it asked for; those nothing
    // else references are disposed and their bytes come off the budget.
    this.textureQueue?.dispose();
    this.textureQueue = null;
    this.textures.clear();
    this.textureUsers.clear();
    this.materialsRequested.clear();
    this.materialsPending.length = 0;

    for (const b of this.batches) {
      b.mesh.removeFromParent();
      // Model geometries are owned by the shared mesh cache and textures by the
      // shared texture cache; placeholder and entity geometries are
      // module-level singletons. Only the per-batch material wrappers and
      // instance buffers belong to us.
      disposeMaterial(b.mesh.material);
      b.mesh.dispose();
    }
    this.batches = [];
    this.batchByKey.clear();
    this.batchByMesh.clear();
    this.slots.clear();
    this.matrices.clear();

    for (const child of [...this.entityGroup.children]) {
      if (child instanceof THREE.Points) {
        child.geometry.dispose();
        (child.material as THREE.Material).dispose();
        child.removeFromParent();
      }
    }

    this.modelInstanceCount = 0;
    this.entityInstanceCount = 0;
    this.uniqueModels = 0;
    this.mirroredInstanceCount = 0;
    this.suspectMeshCount = 0;
    this.blockGeoCount = 0;
    this.blockGeoLoaded = 0;
    this.blockGeoTriangles = 0;
    this.selection = [];
    this.selectionSet.clear();
    this.hovered = null;
    this.sceneBounds.makeEmpty();
    this.gizmo.detach();
    this.outline.visible = false;
  }

  // -- geometry streaming ---------------------------------------------------

  private onGeometry(key: string, geometry: THREE.BufferGeometry | null): void {
    // An empty buffer decodes to null; the batch keeps the placeholder it has
    // (a unit box for a model, the scale-hint wireframe box for a block-2
    // object), which is exactly the fallback those objects had before.
    if (!geometry) return;
    const batch = this.batchByKey.get(key);
    if (!batch || batch.kind === "entity") return;
    if (geometry.userData.windingSuspect) this.suspectMeshCount++;

    if (batch.kind === "blockGeo" && !batch.resolved) {
      // Swap the marker box for the real mesh: solid tinted material, and the
      // object's own untouched 4x4 - the marker matrix substitutes a readable
      // box size for the entry's scale hint, which would squash the real mesh.
      const old = batch.mesh.material as THREE.Material;
      batch.mesh.material = createBlockGeoMaterial(batch.geoKind!, this.shading, this.wireframe);
      old.dispose();
      batch.resolved = true;
      const m = this.matrices.get(batch.objects[0].offset);
      if (m) batch.mesh.setMatrixAt(0, m);
      batch.mesh.instanceMatrix.needsUpdate = true;
      this.refreshColor(batch.objects[0].offset);
      this.blockGeoLoaded++;
      const idx = geometry.getIndex();
      this.blockGeoTriangles += Math.floor((idx ? idx.count : geometry.getAttribute("position").count) / 3);
    }

    batch.mesh.geometry = geometry;
    batch.mesh.computeBoundingSphere();
    // The camera's cached surface distance was measured against the box this
    // mesh just replaced; keeping it would scale the next frame's speed off a
    // placeholder that is no longer on screen.
    batch.probeBounds = null;
    this.rig.invalidateSurface();
    if (batch.kind === "model") {
      applyGroups(geometry, key);
      // The material table may have arrived first; the groups it indexes only
      // exist now, so the array is built (or rebuilt) here.
      this.rebuildBatchMaterials(batch);
    }
    this.updateOutline();
    this.needsRender = true;
  }

  // -- materials and textures -----------------------------------------------

  /**
   * Ask for the material tables of these models, a few at a time.
   *
   * One command per model (memoised per game directory in `api.ts`), so a
   * region with 881 distinct models is 881 small JSON round trips. Bounded
   * concurrency keeps them from competing with the mesh fetches, and the order
   * is the same instance-count order the meshes stream in, so the art that
   * covers the most screen gets its textures first.
   */
  private requestMaterials(modelIds: string[]): void {
    if (!this.textureSource) return;
    for (const id of modelIds) {
      if (this.materialsRequested.has(id)) continue;
      this.materialsRequested.add(id);
      this.materialsPending.push(id);
    }
    this.pumpMaterials();
  }

  private pumpMaterials(): void {
    const src = this.textureSource;
    if (!src) return;
    while (this.materialsInFlight < 6 && this.materialsPending.length > 0) {
      const id = this.materialsPending.shift()!;
      const batch = this.batchByKey.get(id);
      if (!batch || batch.kind !== "model") continue;
      this.materialsInFlight++;
      src
        .materials(id)
        .catch(() => null)
        .then((dto) => {
          // A zone swap between request and answer invalidates the batch.
          if (this.batchByKey.get(id) !== batch) return;
          batch.materials = dto;
          this.rebuildBatchMaterials(batch);
          this.needsRender = true;
        })
        .finally(() => {
          this.materialsInFlight--;
          this.pumpMaterials();
        });
    }
  }

  /**
   * The texture for one id, requesting it the first time it is asked for.
   *
   * Returns null while it is in flight (and for good if it will not decode or
   * the VRAM budget refused it) — the caller falls back to the flat hue, and
   * `onTexture` patches the material in when it lands.
   */
  private textureFor(id: string, user: Batch): THREE.Texture | null {
    let users = this.textureUsers.get(id);
    if (!users) this.textureUsers.set(id, (users = new Set()));
    users.add(user);
    const hit = this.textures.get(id);
    if (hit !== undefined) return hit;
    this.textureQueue?.request(id);
    return null;
  }

  /**
   * Resolve one blend plan against the textures that have actually landed,
   * requesting the ones that have not.
   *
   * A layer whose texture is not here yet comes back null, which
   * {@link updateBlendMaterial} turns into a zero weight for that channel — so
   * the surface shows the base layer, i.e. exactly the picture the
   * single-albedo path drew, and each layer sharpens it as it arrives. Layers
   * are only asked for once the base is resident (see `rebuildBatchMaterials`),
   * which keeps the whole zone's base coverage ahead of any one model's detail.
   */
  private blendInputFor(plan: BlendPlan, batch: Batch): BlendInput {
    const base = this.textureFor(plan.layers[0].textureId, batch);
    return {
      mask: this.textureFor(plan.maskTextureId, batch),
      maskUvScale: plan.maskUvScale,
      layers: plan.layers.map((l, i) => ({
        texture: i === 0 ? base : plan.enabled[i - 1] ? this.textureFor(l.textureId, batch) : null,
        uvScale: l.uvScale,
        hsl: l.hsl,
        blend: l.blend,
        invert: l.invert,
      })),
    };
  }

  /**
   * Fold a newly arrived texture into one already-built material.
   *
   * Returns false when the material is the wrong KIND for what the slot now
   * wants — a blend slot whose base layer has only just landed is still on the
   * plain material it was given while the base was missing — in which case the
   * caller rebuilds the batch. Everything else is a uniform write and needs no
   * recompile.
   */
  private patchSlotMaterial(m: THREE.Material, use: SlotTextures, batch: Batch): boolean {
    if (use.plan) {
      if (isBlendMaterial(m)) {
        updateBlendMaterial(m, this.blendInputFor(use.plan, batch));
        return true;
      }
      // Still on the fallback material. Only the blend's own base layer
      // arriving can promote it; anything else falls through to the plain
      // patch below, so a slot whose base never decodes keeps its albedo.
      if (this.textures.get(use.plan.layers[0].textureId)) return false;
    }
    const tex = use.baseId ? this.textures.get(use.baseId) ?? null : null;
    if (!tex) return true;
    const phong = m as THREE.MeshPhongMaterial;
    phong.map = tex;
    phong.color.setRGB(1, 1, 1);
    phong.needsUpdate = true;
    return true;
  }

  private onTexture(id: string, texture: THREE.Texture | null): void {
    this.textures.set(id, texture);
    const users = this.textureUsers.get(id);
    if (!users) return;
    for (const batch of users) {
      if (this.batchByKey.get(batch.key) !== batch) continue;
      const uses = batch.slotTextures;
      if (!uses) {
        // First texture of this model: it is still on a single flat material,
        // so the array has to be built now.
        if (texture) this.rebuildBatchMaterials(batch);
        continue;
      }
      const mats = batch.mesh.material;
      const list = Array.isArray(mats) ? mats : [mats];
      let rebuild = false;
      for (let i = 0; i < list.length; i++) {
        const use = uses[i];
        if (!use || !usesTexture(use, id)) continue;
        if (!this.patchSlotMaterial(list[i], use, batch)) rebuild = true;
      }
      if (rebuild) this.rebuildBatchMaterials(batch);
    }
    this.needsRender = true;
  }

  /**
   * (Re)build one model batch's material(s) for the current shading mode.
   *
   * Three outcomes, cheapest first:
   *   - not textured, or no material table, or no slot with a usable albedo:
   *     ONE material for the whole batch, exactly as before this change. That
   *     keeps a model at one draw call, which matters when a region has 881 of
   *     them.
   *   - one submesh: one material, carrying that submesh's albedo.
   *   - several submeshes: an array indexed by the geometry groups' material
   *     index, so three draws one call per submesh with its own albedo.
   *
   * A slot that carries a `gbl_rgb_blend` recipe (71% of a Nefarious City art
   * tile's slots) takes the blend material instead of the single albedo, but
   * only once its BASE layer is resident: until then it falls through to the
   * plain path, which is the same map drawn flat, so the picture only ever
   * improves as textures land. `onTexture` promotes it when the base arrives
   * and feeds the remaining layers in by uniform after that.
   */
  private rebuildBatchMaterials(batch: Batch): void {
    if (batch.kind !== "model") return;
    const tint = colorForKey(batch.key);
    const flat = () => {
      batch.slotTextures = undefined;
      batch.blended = 0;
      this.setBatchMaterial(batch, createModelMaterial(this.shading, this.wireframe, { tint }));
    };

    const slots = batch.materials?.slots;
    if (!usesTextures(this.shading) || !slots || slots.length === 0) return flat();

    const subs = submeshTables.get(batch.key);
    const byIndex = new Map(slots.map((s) => [s.index, s]));
    let blended = 0;
    const makeFor = (materialIndex: number): { material: THREE.Material; use: SlotTextures } => {
      const slot = byIndex.get(materialIndex);
      const plan = slot ? blendPlanOf(slot) : null;
      if (plan && this.textureFor(plan.layers[0].textureId, batch)) {
        // The base layer is the gate: only once it is here do we reach for the
        // mask and the other three, so a zone reaches full base coverage before
        // any one model starts refining itself.
        blended++;
        return {
          material: createModelMaterial(this.shading, this.wireframe, {
            tint,
            blend: this.blendInputFor(plan, batch),
          }),
          use: { baseId: plan.layers[0].textureId, plan },
        };
      }
      const albedo = slot ? albedoOf(slot) : null;
      const id = albedo?.textureId ?? null;
      const map = id ? this.textureFor(id, batch) : null;
      return {
        material: createModelMaterial(this.shading, this.wireframe, { tint, map }),
        use: plan ? { baseId: id, plan } : { baseId: id },
      };
    };

    // One submesh (or an unknown table): a single material, no groups needed.
    if (!subs || subs.length < 2) {
      const only = makeFor(subs?.[0]?.materialIndex ?? slots[0].index);
      if (!only.use.baseId || !this.textures.get(only.use.baseId)) {
        only.material.dispose();
        return flat();
      }
      batch.slotTextures = [only.use];
      batch.blended = blended;
      this.setBatchMaterial(batch, only.material);
      return;
    }

    // Several submeshes: the geometry must already carry the matching groups.
    const groups = batch.mesh.geometry.groups;
    if (groups.length === 0) return flat();

    let highest = 0;
    for (const g of groups) highest = Math.max(highest, g.materialIndex ?? 0);
    const built: THREE.Material[] = [];
    const uses: SlotTextures[] = [];
    let textured = 0;
    for (let i = 0; i <= highest; i++) {
      const made = makeFor(i);
      built.push(made.material);
      uses.push(made.use);
      if (made.use.baseId && this.textures.get(made.use.baseId)) textured++;
    }
    if (textured === 0) {
      // Nothing to show yet — stay on one draw call until a texture lands.
      for (const m of built) m.dispose();
      return flat();
    }
    batch.slotTextures = uses;
    batch.blended = blended;
    this.setBatchMaterial(batch, built);
  }

  private setBatchMaterial(batch: Batch, material: THREE.Material | THREE.Material[]): void {
    const old = batch.mesh.material;
    batch.mesh.material = material;
    // Textures belong to the shared cache and are NOT disposed here — only the
    // material wrappers, which are per batch.
    disposeMaterial(old);
    this.needsRender = true;
  }

  /**
   * The per-instance colour a model batch sits at when nothing is selected.
   *
   * In `textured` mode that is white, because the instance colour multiplies
   * the albedo; the per-model hue moves into `material.color` for the submeshes
   * that have no texture (see `ModelMaterialOptions.tint`). Selection and hover
   * keep using the instance colour in every mode, so a selected instance still
   * turns blue over its texture.
   */
  private baseInstanceColor(modelId: string): THREE.Color {
    return usesTextures(this.shading) ? new THREE.Color(0xffffff) : colorForKey(modelId);
  }

  /** Re-stamp every model batch's base instance colour after a mode change. */
  private applyBaseColors(): void {
    for (const b of this.batches) {
      if (b.kind !== "model") continue;
      const c = this.baseInstanceColor(b.key);
      for (let i = 0; i < b.objects.length; i++) b.mesh.setColorAt(i, c);
      if (b.mesh.instanceColor) b.mesh.instanceColor.needsUpdate = true;
    }
    for (const off of this.selectionSet) this.refreshColor(off);
    if (this.hovered !== null) this.refreshColor(this.hovered);
  }

  // -- helpers --------------------------------------------------------------

  private buildHelpers(bounds: THREE.Box3): void {
    this.grid?.removeFromParent();
    this.grid?.dispose();
    this.axes?.removeFromParent();
    this.axes?.dispose();

    const size = bounds.getSize(new THREE.Vector3());
    const span = Math.max(size.x, size.z, 20);
    // Round up to a decade-ish grid so the divisions stay ~10 m at tile scale.
    const gridSize = Math.pow(2, Math.ceil(Math.log2(span * 1.5)));
    const divisions = THREE.MathUtils.clamp(Math.round(gridSize / 10), 10, 200);

    this.grid = new THREE.GridHelper(gridSize, divisions, 0x3d4757, 0x232a36);
    const center = bounds.getCenter(new THREE.Vector3());
    this.grid.position.set(center.x, bounds.min.y, center.z);
    (this.grid.material as THREE.Material).transparent = true;
    (this.grid.material as THREE.Material).opacity = 0.7;
    this.helperGroup.add(this.grid);

    this.axes = new THREE.AxesHelper(Math.max(5, span * 0.06));
    this.axes.position.set(center.x, bounds.min.y + 0.02, center.z);
    this.helperGroup.add(this.axes);
  }

  private entityMarkerMatrix(o: SceneObjectDto, m: THREE.Matrix4): THREE.Matrix4 {
    // The zone's `scale` triple is an extent hint; fall back to a readable size.
    const s = o.scale;
    const dim = (v: number) => (Number.isFinite(v) && v > 0.01 ? THREE.MathUtils.clamp(v * 2, 0.5, 400) : 2);
    const sx = dim(s?.[0] ?? 0);
    const sy = dim(s?.[1] ?? 0);
    const sz = dim(s?.[2] ?? 0);
    // Strip the object's own scale so the marker keeps its intended size.
    const pos = new THREE.Vector3();
    const quat = new THREE.Quaternion();
    const scl = new THREE.Vector3();
    m.decompose(pos, quat, scl);
    return new THREE.Matrix4().compose(pos, quat, new THREE.Vector3(sx, sy, sz));
  }

  private writeInstanceMatrix(slot: Slot, m: THREE.Matrix4): void {
    if (isMarkerBox(slot.batch)) {
      slot.batch.mesh.setMatrixAt(slot.index, this.entityMarkerMatrix(slot.object, m.clone()));
    } else {
      slot.batch.mesh.setMatrixAt(slot.index, m);
    }
    slot.batch.mesh.instanceMatrix.needsUpdate = true;
    slot.batch.probeBounds = null; // the camera probe's prefilter is now stale
  }

  private refreshColor(offset: number): void {
    const slot = this.slots.get(offset);
    if (!slot) return;
    const { batch, index, object } = slot;
    let color: THREE.Color;
    if (this.selectionSet.has(offset)) color = SELECT_COLOR;
    else if (this.hovered === offset) color = HOVER_COLOR;
    // In textured mode the kind tint would be the only saturated thing on
    // screen, so block-2 meshes go neutral and read as untextured geometry.
    else if (batch.kind === "blockGeo")
      color = usesTextures(this.shading)
        ? new THREE.Color(BLOCK_GEO_NEUTRAL)
        : new THREE.Color(BLOCK_GEO_TINTS[batch.geoKind!]);
    else if (batch.kind === "entity") color = new THREE.Color(KIND_COLORS[object.kind] ?? KIND_FALLBACK);
    else color = this.baseInstanceColor(batch.key);
    batch.mesh.setColorAt(index, color);
    if (batch.mesh.instanceColor) batch.mesh.instanceColor.needsUpdate = true;
  }

  private objectBox(offset: number): THREE.Box3 | null {
    const slot = this.slots.get(offset);
    const m = this.matrices.get(offset);
    if (!slot || !m) return null;
    let local: THREE.Box3;
    if (isMarkerBox(slot.batch)) {
      const s = slot.object.scale;
      const dim = (v: number) => (Number.isFinite(v) && v > 0.01 ? THREE.MathUtils.clamp(v, 0.25, 200) : 1);
      local = new THREE.Box3(
        new THREE.Vector3(-dim(s?.[0] ?? 0), -dim(s?.[1] ?? 0), -dim(s?.[2] ?? 0)),
        new THREE.Vector3(dim(s?.[0] ?? 0), dim(s?.[1] ?? 0), dim(s?.[2] ?? 0)),
      );
      const pos = new THREE.Vector3().setFromMatrixPosition(m);
      return local.translate(pos);
    }
    const g = slot.batch.mesh.geometry;
    if (!g.boundingBox) g.computeBoundingBox();
    local = g.boundingBox!.clone();
    return local.applyMatrix4(m);
  }

  private selectionBox(): THREE.Box3 | null {
    if (!this.selection.length) return null;
    const box = new THREE.Box3();
    for (const off of this.selection) {
      const b = this.objectBox(off);
      if (b) box.union(b);
    }
    return box.isEmpty() ? null : box;
  }

  private updateOutline(): void {
    const verts: number[] = [];
    const max = Math.min(this.selection.length, 256);
    for (let i = 0; i < max; i++) {
      const b = this.objectBox(this.selection[i]);
      if (!b) continue;
      pushBoxEdges(verts, b);
    }
    this.outline.geometry.dispose();
    const g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.Float32BufferAttribute(verts, 3));
    this.outline.geometry = g;
    this.outline.visible = verts.length > 0;
  }

  // -- gizmo ----------------------------------------------------------------

  private bindGizmo(): void {
    this.gizmo.addEventListener("dragging-changed", (e) => {
      this.gizmoDragging = Boolean((e as unknown as { value: boolean }).value);
      if (this.gizmoDragging) this.beginGizmoDrag();
      else this.endGizmoDrag();
      this.needsRender = true;
    });
    this.gizmo.addEventListener("objectChange", () => {
      this.applyGizmoDelta();
      this.needsRender = true;
    });
    this.gizmo.addEventListener("change", () => {
      this.needsRender = true;
    });
  }

  private updateGizmoAttachment(): void {
    const usable = this.gizmoMode !== null && this.selection.length > 0;
    if (!usable) {
      this.gizmo.detach();
      return;
    }
    if (this.gizmoDragging) return;

    // Anchor at the first selected object's transform so "local" space works,
    // but sit at the centroid of the whole selection.
    const first = this.matrices.get(this.selection[0]);
    const pos = new THREE.Vector3();
    const quat = new THREE.Quaternion();
    const scl = new THREE.Vector3(1, 1, 1);
    if (first) first.decompose(pos, quat, scl);

    if (this.selection.length > 1) {
      const c = new THREE.Vector3();
      let n = 0;
      for (const off of this.selection) {
        const m = this.matrices.get(off);
        if (!m) continue;
        c.add(new THREE.Vector3().setFromMatrixPosition(m));
        n++;
      }
      if (n) pos.copy(c.divideScalar(n));
    }

    this.gizmoProxy.position.copy(pos);
    this.gizmoProxy.quaternion.copy(quat);
    this.gizmoProxy.scale.set(1, 1, 1);
    this.gizmoProxy.updateMatrixWorld(true);
    this.gizmo.attach(this.gizmoProxy);
  }

  private beginGizmoDrag(): void {
    this.gizmoProxy.updateMatrixWorld(true);
    const offsets = this.selection.filter((o) => this.matrices.has(o));
    this.dragSnapshot = {
      offsets,
      start: offsets.map((o) => this.matrices.get(o)!.clone()),
      proxyInv: this.gizmoProxy.matrixWorld.clone().invert(),
    };
  }

  private applyGizmoDelta(): void {
    const snap = this.dragSnapshot;
    if (!snap) return;
    this.gizmoProxy.updateMatrixWorld(true);
    const delta = new THREE.Matrix4().multiplyMatrices(this.gizmoProxy.matrixWorld, snap.proxyInv);
    for (let i = 0; i < snap.offsets.length; i++) {
      const off = snap.offsets[i];
      const m = new THREE.Matrix4().multiplyMatrices(delta, snap.start[i]);
      this.matrices.set(off, m);
      const slot = this.slots.get(off);
      if (slot) this.writeInstanceMatrix(slot, m);
    }
    this.updateOutline();
  }

  private endGizmoDrag(): void {
    const snap = this.dragSnapshot;
    this.dragSnapshot = null;
    if (!snap) return;
    for (const b of this.batches) b.mesh.computeBoundingSphere();
    if (this.opts.onTransform) {
      for (const off of snap.offsets) {
        const m = this.matrices.get(off);
        if (m) this.opts.onTransform(off, m.toArray());
      }
    }
    this.updateGizmoAttachment();
  }

  // -- picking --------------------------------------------------------------

  private ndc(clientX: number, clientY: number): THREE.Vector2 {
    const r = this.renderer.domElement.getBoundingClientRect();
    return new THREE.Vector2(((clientX - r.left) / r.width) * 2 - 1, -((clientY - r.top) / r.height) * 2 + 1);
  }

  /**
   * Distance from `origin` along `dir` to the first drawn surface, or null.
   *
   * This is the camera rig's speed reference. Targets are geometry batches
   * only: an entity marker box is navigation furniture, not a wall, and
   * scaling the fly speed to one would stall the camera in open air. Block-2
   * batches ARE included - on an impostor zone they are the whole skyline.
   *
   * The rig caches the answer, rate-limits the call and carries the hit forward
   * as the camera travels, so this runs at most ~20 times a second however hard
   * the camera is driven; the measured cost per call is in the HUD (`probe`).
   */
  private surfaceProbe = (origin: THREE.Vector3, dir: THREE.Vector3, maxDist: number): number | null => {
    if (!this.showGeometry) return null;

    const far = Math.max(maxDist, 1e-3);
    const rc = this.probeRaycaster;
    rc.camera = this.camera;
    rc.near = 0;
    rc.far = far;
    rc.ray.origin.copy(origin);
    const rd = rc.ray.direction.copy(dir).normalize();
    const ox = origin.x;
    const oy = origin.y;
    const oz = origin.z;

    // Stage 1: per-INSTANCE sphere prefilter, in plain floats.
    //
    // `Raycaster.intersectObjects` cannot do this job: InstancedMesh.raycast
    // walks every instance of every batch whose own (tile-sized) bounding
    // sphere the ray crosses, paying a matrix multiply and an inverse each
    // time, and `far` only spares it the triangle loop. Measured on savali
    // tile_c17_art (9,917 instances, 91 block-2 meshes, 346,820 triangles) at
    // a metre from a wall: 7.3 ms unbounded, 6.6 ms even with a 50 m window.
    // The quads below cost one dot product and a compare per instance.
    const cands = this.probeCandidates;
    cands.length = 0;
    for (const b of this.batches) {
      if (b.kind === "entity" || !this.batchVisible(b)) continue;
      const bounds = b.probeBounds ?? this.buildProbeBounds(b);
      if (!bounds) continue;
      for (let i = 0, q = 0; q < bounds.length; i++, q += 4) {
        const r = bounds[q + 3];
        if (r <= 0) continue;
        const dx = bounds[q] - ox;
        const dy = bounds[q + 1] - oy;
        const dz = bounds[q + 2] - oz;
        const along = dx * rd.x + dy * rd.y + dz * rd.z;
        if (along - r > far || along + r < 0) continue; // too far, or behind
        const perp2 = dx * dx + dy * dy + dz * dz - along * along;
        if (perp2 > r * r) continue; // the ray misses this instance's sphere
        cands.push({ batch: b, index: i, near: Math.max(0, along - r) });
      }
    }
    if (!cands.length) return null;

    // Stage 2: exact triangles, nearest candidate first, stopping as soon as no
    // remaining candidate's sphere could start closer than the best hit.
    cands.sort((a, b) => a.near - b.near);
    const mesh = this.probeMesh;
    const hits = this.probeHits;
    let best = Infinity;
    for (const c of cands) {
      if (c.near >= best) break;
      mesh.geometry = c.batch.mesh.geometry;
      // A DOUBLE-SIDED stand-in on purpose: the drawn materials are
      // single-sided, and three honours `material.side` when raycasting, so
      // from inside a building every wall the ray would hit is back-facing and
      // the probe came back empty - which handed the fly speed the orbit
      // radius and 204 m/s in a corridor.
      // Read back the matrix the batch is DRAWN with, not the scene object's
      // own 4x4: an unresolved block-2 object wears a substituted marker-box
      // transform, and the probe has to agree with what is on screen.
      c.batch.mesh.getMatrixAt(c.index, mesh.matrixWorld);
      hits.length = 0;
      mesh.raycast(rc, hits);
      for (const h of hits) if (h.distance > 0 && h.distance < best) best = h.distance;
    }
    return best === Infinity ? null : best;
  };

  /** World bounding sphere per instance as [cx, cy, cz, r] quads. */
  private buildProbeBounds(b: Batch): Float32Array | null {
    const geometry = b.mesh.geometry;
    if (!geometry.boundingSphere) geometry.computeBoundingSphere();
    const sphere = geometry.boundingSphere;
    if (!sphere) return null;
    const out = new Float32Array(b.objects.length * 4);
    const c = this.probeVec;
    const m = this.probeMatrix;
    for (let i = 0, q = 0; i < b.objects.length; i++, q += 4) {
      b.mesh.getMatrixAt(i, m);
      const e = m.elements;
      // Largest basis-column length: the scale the sphere radius has to survive.
      const sx = Math.hypot(e[0], e[1], e[2]);
      const sy = Math.hypot(e[4], e[5], e[6]);
      const sz = Math.hypot(e[8], e[9], e[10]);
      const scale = Math.max(sx, sy, sz);
      // An instance slot nobody wrote is all zeros; leave its radius at 0, which
      // the probe skips.
      if (!(scale > 0) || !Number.isFinite(e[12] + e[13] + e[14])) continue;
      c.copy(sphere.center).applyMatrix4(m);
      out[q] = c.x;
      out[q + 1] = c.y;
      out[q + 2] = c.z;
      out[q + 3] = sphere.radius * scale;
    }
    b.probeBounds = out;
    return out;
  }

  private pickAt(clientX: number, clientY: number): number | null {
    this.pickPointValid = false;
    const targets: THREE.Object3D[] = [];
    for (const b of this.batches) if (this.batchVisible(b)) targets.push(b.mesh);
    if (!targets.length) return null;

    this.raycaster.setFromCamera(this.ndc(clientX, clientY), this.camera);
    const hits = this.raycaster.intersectObjects(targets, false);

    // Rank by the same biased depth the shader writes, not the raw distance.
    // A decal sits ~2 cm in front of the surface it decorates once biased but
    // ~2 cm behind it geometrically (measured on savali `tile_c17_art`: model
    // hit at 19.18 m, decal at 19.20 m), so an unbiased raycast would hand back
    // the wall the user can no longer see instead of the decal drawn over it.
    let best: number | null = null;
    let bestDepth = Infinity;
    for (const h of hits) {
      const batch = this.batchByMesh.get(h.object);
      if (!batch) continue;
      const id = h.instanceId;
      if (id === undefined || id < 0 || id >= batch.objects.length) continue;
      const d = h.distance * (1 - depthBiasFor(batch.kind === "blockGeo" ? batch.geoKind! : null));
      if (d < bestDepth) {
        bestDepth = d;
        best = batch.objects[id].offset;
        this.pickPoint.copy(h.point);
        this.pickPointValid = true;
      }
    }
    if (best !== null) return best;

    // Entities are often sub-metre; fall back to a screen-space proximity test
    // so markers stay clickable when they are only a few pixels across.
    if (this.showEntities) return this.pickEntityScreenSpace(clientX, clientY, 12);
    return null;
  }

  private pickEntityScreenSpace(clientX: number, clientY: number, radiusPx: number): number | null {
    const r = this.renderer.domElement.getBoundingClientRect();
    const px = clientX - r.left;
    const py = clientY - r.top;
    const v = new THREE.Vector3();
    let best: number | null = null;
    let bestScore = Infinity;

    for (const b of this.batches) {
      if (b.kind !== "entity") continue;
      for (const o of b.objects) {
        const m = this.matrices.get(o.offset);
        if (!m) continue;
        v.setFromMatrixPosition(m).project(this.camera);
        if (v.z < -1 || v.z > 1) continue;
        const sx = ((v.x + 1) / 2) * r.width;
        const sy = ((1 - v.y) / 2) * r.height;
        const d = Math.hypot(sx - px, sy - py);
        if (d > radiusPx) continue;
        const score = d + v.z * 4; // prefer nearer objects at similar distance
        if (score < bestScore) {
          bestScore = score;
          best = o.offset;
        }
      }
    }
    return best;
  }

  private pickInRect(x0: number, y0: number, x1: number, y1: number): number[] {
    const r = this.renderer.domElement.getBoundingClientRect();
    const minX = Math.min(x0, x1) - r.left;
    const maxX = Math.max(x0, x1) - r.left;
    const minY = Math.min(y0, y1) - r.top;
    const maxY = Math.max(y0, y1) - r.top;
    const v = new THREE.Vector3();
    const out: number[] = [];

    for (const b of this.batches) {
      if (!this.batchVisible(b)) continue;
      for (const o of b.objects) {
        const m = this.matrices.get(o.offset);
        if (!m) continue;
        v.setFromMatrixPosition(m).project(this.camera);
        if (v.z < -1 || v.z > 1) continue;
        const sx = ((v.x + 1) / 2) * r.width;
        const sy = ((1 - v.y) / 2) * r.height;
        if (sx >= minX && sx <= maxX && sy >= minY && sy <= maxY) out.push(o.offset);
      }
    }
    return out;
  }

  private setHovered(offset: number | null): void {
    if (this.hovered === offset) return;
    const prev = this.hovered;
    this.hovered = offset;
    if (prev !== null) this.refreshColor(prev);
    if (offset !== null) this.refreshColor(offset);
    this.needsRender = true;
  }

  // -- input ----------------------------------------------------------------

  private onPointerDown = (e: PointerEvent) => {
    this.renderer.domElement.focus();
    if (this.gizmo.axis !== null && this.gizmo.object) return; // gizmo owns this drag

    this.dragButton = e.button;
    this.dragMoved = false;
    this.dragStart = { x: e.clientX, y: e.clientY };
    this.lastPointer = { x: e.clientX, y: e.clientY };
    this.boxSelecting = e.button === 0 && e.ctrlKey;
    if (this.boxSelecting) this.opts.onBoxSelect?.({ x: e.clientX, y: e.clientY, w: 0, h: 0 });
    // Orbit should pivot around what is on screen, not around wherever the last
    // frame-selection left the pivot. One probe per drag, down the view axis so
    // the camera neither moves nor rotates when the pivot is re-anchored.
    else if (e.button === 0) this.rig.recenterPivot();
    try {
      this.renderer.domElement.setPointerCapture(e.pointerId);
    } catch {
      /* synthetic or already-released pointer */
    }
  };

  private onPointerMove = (e: PointerEvent) => {
    if (this.dragButton < 0) {
      const now = performance.now();
      if (now - this.lastHover > 60 && this.modelInstanceCount + this.entityInstanceCount <= HOVER_INSTANCE_LIMIT) {
        this.lastHover = now;
        this.setHovered(this.pickAt(e.clientX, e.clientY));
      }
      return;
    }

    const dx = e.clientX - this.lastPointer.x;
    const dy = e.clientY - this.lastPointer.y;
    this.lastPointer = { x: e.clientX, y: e.clientY };
    if (Math.abs(e.clientX - this.dragStart.x) + Math.abs(e.clientY - this.dragStart.y) > 3) this.dragMoved = true;

    if (this.boxSelecting) {
      this.opts.onBoxSelect?.({
        x: Math.min(this.dragStart.x, e.clientX),
        y: Math.min(this.dragStart.y, e.clientY),
        w: Math.abs(e.clientX - this.dragStart.x),
        h: Math.abs(e.clientY - this.dragStart.y),
      });
      return;
    }

    if (this.dragButton === 0) this.rig.orbit(dx, dy);
    else this.rig.pan(dx, dy, this.renderer.domElement.clientHeight);
  };

  private onPointerUp = (e: PointerEvent) => {
    if (this.dragButton < 0) return;
    try {
      this.renderer.domElement.releasePointerCapture(e.pointerId);
    } catch {
      /* pointer already released */
    }

    if (this.boxSelecting) {
      this.opts.onBoxSelect?.(null);
      if (this.dragMoved) {
        const hits = this.pickInRect(this.dragStart.x, this.dragStart.y, e.clientX, e.clientY);
        this.opts.onSelect(hits, e.shiftKey);
      }
    } else if (this.dragButton === 0 && !this.dragMoved) {
      const hit = this.pickAt(e.clientX, e.clientY);
      // Clicking a surface puts the pivot on it (projected onto the view axis,
      // so nothing jumps): the wheel step, the pan rate and the next orbit all
      // then work at the scale of the thing the user just pointed at.
      if (hit !== null && this.pickPointValid) this.rig.setPivotAtPoint(this.pickPoint);
      this.opts.onSelect(hit === null ? [] : [hit], e.shiftKey);
    }

    this.dragButton = -1;
    this.boxSelecting = false;
  };

  private onWheel = (e: WheelEvent) => {
    e.preventDefault();
    // Line-mode wheels report ~3 lines where pixel-mode reports ~100 px; without
    // the conversion a notch would move the camera 0.6% instead of 18%.
    const delta = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaMode === 2 ? e.deltaY * 400 : e.deltaY;
    this.rig.dolly(delta, this.ndc(e.clientX, e.clientY));
  };

  private onContextMenu = (e: Event) => e.preventDefault();

  private onPointerLeave = () => this.setHovered(null);

  private bindPointer(): void {
    const el = this.renderer.domElement;
    el.addEventListener("pointerdown", this.onPointerDown);
    el.addEventListener("pointermove", this.onPointerMove);
    el.addEventListener("pointerup", this.onPointerUp);
    el.addEventListener("pointerleave", this.onPointerLeave);
    el.addEventListener("wheel", this.onWheel, { passive: false });
    el.addEventListener("contextmenu", this.onContextMenu);
  }

  private unbindPointer(): void {
    const el = this.renderer.domElement;
    el.removeEventListener("pointerdown", this.onPointerDown);
    el.removeEventListener("pointermove", this.onPointerMove);
    el.removeEventListener("pointerup", this.onPointerUp);
    el.removeEventListener("pointerleave", this.onPointerLeave);
    el.removeEventListener("wheel", this.onWheel);
    el.removeEventListener("contextmenu", this.onContextMenu);
  }

  private onKeyDown = (e: KeyboardEvent) => {
    // Ctrl/Cmd chords are app shortcuts (Ctrl+D duplicates); only Alt, the slow modifier, flies.
    if (e.ctrlKey || e.metaKey) return;
    if (e.altKey) {
      this.rig.setKey(e.code, true);
      return;
    }
    switch (e.code) {
      case "KeyF":
        e.preventDefault();
        this.frameSelection();
        return;
      case "KeyH":
        e.preventDefault();
        this.frameAll();
        return;
      case "KeyG":
        this.setGizmoMode("translate");
        return;
      case "KeyR":
        this.setGizmoMode("rotate");
        return;
      case "KeyT":
        this.setGizmoMode("scale");
        return;
      case "Escape":
        this.opts.onSelect([], false);
        return;
      default:
        this.rig.setKey(e.code, true);
    }
  };

  private onKeyUp = (e: KeyboardEvent) => this.rig.setKey(e.code, false);
  private onBlur = () => this.rig.clearKeys();

  private bindKeyboard(): void {
    const el = this.renderer.domElement;
    el.tabIndex = 0;
    el.addEventListener("keydown", this.onKeyDown);
    el.addEventListener("keyup", this.onKeyUp);
    el.addEventListener("blur", this.onBlur);
  }

  private unbindKeyboard(): void {
    const el = this.renderer.domElement;
    el.removeEventListener("keydown", this.onKeyDown);
    el.removeEventListener("keyup", this.onKeyUp);
    el.removeEventListener("blur", this.onBlur);
  }

  // -- frame loop -----------------------------------------------------------

  private resize(): void {
    const w = Math.max(1, this.container.clientWidth);
    const h = Math.max(1, this.container.clientHeight);
    this.renderer.setSize(w, h, false);
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();
    this.needsRender = true;
  }

  private loop = () => {
    if (!this.running) return;
    this.rafId = requestAnimationFrame(this.loop);

    const now = performance.now();
    const dt = Math.min(0.1, (now - this.lastTime) / 1000);
    this.lastTime = now;

    this.rig.update(dt);
    if (this.rig.takeDirty()) this.needsRender = true;

    // Render every frame while the pane is visible: the scene is a handful of
    // instanced draw calls, and a continuous loop keeps the FPS readout honest.
    // When the window is hidden we fall back to drawing only on real changes.
    const visible = !document.hidden && this.container.clientWidth > 0;
    if (visible || this.needsRender) {
      this.needsRender = false;
      this.renderer.render(this.scene, this.camera);
      this.frameTimes.push(now);
      while (this.frameTimes.length > 120) this.frameTimes.shift();
    }

    if (now - this.lastHud > 200) {
      this.lastHud = now;
      this.emitHud(now);
    }
  };

  private emitHud(now: number): void {
    let fps = 0;
    if (this.frameTimes.length > 1) {
      const span = now - this.frameTimes[0];
      if (span > 0) fps = ((this.frameTimes.length - 1) / span) * 1000;
    }
    // Frames older than a second do not count toward the current rate.
    while (this.frameTimes.length && now - this.frameTimes[0] > 1000) this.frameTimes.shift();

    const info = this.renderer.info.render;
    const tex = textureCacheStats(this.textureQueue?.outstanding ?? 0);
    let texturedModels = 0;
    let blendedSubmeshes = 0;
    for (const b of this.batches) {
      if (b.kind !== "model" || !b.slotTextures) continue;
      texturedModels++;
      blendedSubmeshes += b.blended ?? 0;
    }
    const hoveredObj = this.hovered !== null ? this.slots.get(this.hovered) : undefined;
    const selectedObj = this.selection.length ? this.slots.get(this.selection[0]) : undefined;

    // Keep the surface reading live (and the rig's cache warm for the next
    // orbit or wheel), but only while the camera is actually moving: a probe
    // costs milliseconds on a dense zone (measured 8.0 ms on savali
    // tile_c17_art at close range), and a parked camera's cached hit is still
    // exactly right, so paying 5 Hz for it while nothing moves is pure waste.
    if (!this.rig.surfaceKnown || !this.camera.position.equals(this.lastHudCamera)) {
      this.rig.refreshSurfaceProbe();
    }
    this.lastHudCamera.copy(this.camera.position);
    const nav = this.rig.navInfo();

    this.opts.onHud({
      fps: Math.round(fps),
      drawCalls: info.calls,
      triangles: info.triangles,
      instances: this.modelInstanceCount,
      entities: this.entityInstanceCount,
      models: this.uniqueModels,
      loading: this.meshQueue?.outstanding ?? 0,
      cameraPos: [this.camera.position.x, this.camera.position.y, this.camera.position.z],
      hovered: hoveredObj ? describe(hoveredObj.object) : null,
      selectedCount: this.selection.length,
      selectedName: selectedObj ? describe(selectedObj.object) : null,
      mirrored: this.mirroredInstanceCount,
      windingSuspect: this.suspectMeshCount,
      blockGeo: this.blockGeoCount,
      blockGeoLoaded: this.blockGeoLoaded,
      blockGeoTriangles: this.blockGeoTriangles,
      textures: tex.count,
      textureBytes: tex.bytes,
      textureLoading: tex.loading + this.materialsPending.length + this.materialsInFlight,
      textureSkipped: tex.skipped,
      texturedModels,
      blendedSubmeshes,
      surfaceDist: nav.surface,
      moveSpeed: nav.speed,
      probeMs: nav.probeMs,
    });
  }
}

// ---------------------------------------------------------------------------

/**
 * Determinant of the 3x3 basis of a ROW-MAJOR zone matrix. Negative means the
 * instance is mirrored — see `materials.ts` for why that matters.
 */
function determinant3(m: ArrayLike<number>): number {
  return (
    m[0] * (m[5] * m[10] - m[6] * m[9]) -
    m[1] * (m[4] * m[10] - m[6] * m[8]) +
    m[2] * (m[4] * m[9] - m[5] * m[8])
  );
}

function describe(o: SceneObjectDto): string {
  if (o.name) return o.name;
  if (o.kind === KIND_MODEL) return `model ${o.modelId}`;
  return `${o.kindName} @${o.offset}`;
}

function pushBoxEdges(out: number[], b: THREE.Box3): void {
  const { min, max } = b;
  const c: [number, number, number][] = [
    [min.x, min.y, min.z],
    [max.x, min.y, min.z],
    [max.x, min.y, max.z],
    [min.x, min.y, max.z],
    [min.x, max.y, min.z],
    [max.x, max.y, min.z],
    [max.x, max.y, max.z],
    [min.x, max.y, max.z],
  ];
  const edges = [0, 1, 1, 2, 2, 3, 3, 0, 4, 5, 5, 6, 6, 7, 7, 4, 0, 4, 1, 5, 2, 6, 3, 7];
  for (const i of edges) out.push(c[i][0], c[i][1], c[i][2]);
}
