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
  createBlockGeoMaterial,
  createLightRig,
  depthBiasFor,
  createModelMaterial,
  type BlockGeoKind,
  type ShadingMode,
} from "./materials";
import {
  KIND_DECAL,
  KIND_IMPOSTOR,
  KIND_MODEL,
  zoneGeometryKey,
  type GizmoMode,
  type MeshLoader,
  type SceneObjectDto,
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

  private selection: number[] = [];
  private selectionSet = new Set<number>();
  private hovered: number | null = null;

  private showGeometry = true;
  private showEntities = true;
  private wireframe = false;
  private shading: ShadingMode = "lit";

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
    this.renderer.toneMappingExposure = 1.05;
    this.renderer.domElement.style.display = "block";
    this.renderer.domElement.style.width = "100%";
    this.renderer.domElement.style.height = "100%";
    this.renderer.domElement.style.outline = "none";
    this.container.appendChild(this.renderer.domElement);

    // Tiles are hundreds of metres across; a log depth buffer keeps 0.1..20000
    // free of z-fighting.
    this.camera = new THREE.PerspectiveCamera(55, 1, 0.1, 20000);
    this.rig = new CameraRig(this.camera);

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
  setScene(scene: ZoneScene | null, meshLoader: MeshLoader): void {
    const previousPath = this.currentPath;
    this.clearScene();
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
        createModelMaterial(this.shading, this.wireframe),
        objects.length,
      );
      mesh.name = modelId;
      mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
      const base = colorForKey(modelId);
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
    this.meshQueue = new MeshQueue(meshLoader, (key, geom) => this.onGeometry(key, geom), 4);
    for (const modelId of order) this.meshQueue.request(modelId);
    for (const o of blockGeo) this.meshQueue.request(geoKeyOf(o));

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
        (b.mesh.material as THREE.MeshPhongMaterial).wireframe = this.wireframe;
      }
    }
    this.applyGroupVisibility();
    this.updateGizmoAttachment();
    this.needsRender = true;
  }

  /** Swap the model-batch material (lit / normals / facing). */
  setShadingMode(mode: ShadingMode): void {
    if (this.shading === mode) return;
    this.shading = mode;
    for (const b of this.batches) {
      if (isMarkerBox(b)) continue;
      const old = b.mesh.material as THREE.Material;
      b.mesh.material =
        b.kind === "blockGeo"
          ? createBlockGeoMaterial(b.geoKind!, this.shading, this.wireframe)
          : createModelMaterial(this.shading, this.wireframe);
      old.dispose();
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

  /** Push a new matrix for one object (e.g. after an external edit / undo). */
  applyMatrix(offset: number, rowMajor: number[]): void {
    const slot = this.slots.get(offset);
    if (!slot) return;
    const m = zoneToMatrix4(rowMajor);
    this.matrices.set(offset, m);
    this.writeInstanceMatrix(slot, m);
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
    this.meshQueue?.dispose();
    this.meshQueue = null;

    for (const b of this.batches) {
      b.mesh.removeFromParent();
      // Model geometries are owned by the shared mesh cache; placeholder and
      // entity geometries are module-level singletons. Only the per-batch
      // material and instance buffers belong to us.
      (b.mesh.material as THREE.Material).dispose();
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
    this.updateOutline();
    this.needsRender = true;
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
  }

  private refreshColor(offset: number): void {
    const slot = this.slots.get(offset);
    if (!slot) return;
    const { batch, index, object } = slot;
    let color: THREE.Color;
    if (this.selectionSet.has(offset)) color = SELECT_COLOR;
    else if (this.hovered === offset) color = HOVER_COLOR;
    else if (batch.kind === "blockGeo") color = new THREE.Color(BLOCK_GEO_TINTS[batch.geoKind!]);
    else if (batch.kind === "entity") color = new THREE.Color(KIND_COLORS[object.kind] ?? KIND_FALLBACK);
    else color = colorForKey(batch.key);
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

  private pickAt(clientX: number, clientY: number): number | null {
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
      this.opts.onSelect(hit === null ? [] : [hit], e.shiftKey);
    }

    this.dragButton = -1;
    this.boxSelecting = false;
  };

  private onWheel = (e: WheelEvent) => {
    e.preventDefault();
    this.rig.dolly(e.deltaY);
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
    if (e.ctrlKey || e.metaKey || e.altKey) {
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
    const hoveredObj = this.hovered !== null ? this.slots.get(this.hovered) : undefined;
    const selectedObj = this.selection.length ? this.slots.get(this.selection[0]) : undefined;

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
