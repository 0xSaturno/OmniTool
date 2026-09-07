/**
 * Mock data source for the level viewport.
 *
 * Synthesises a `ZoneScene` at roughly the density of a real art tile (a few
 * thousand model instances across a handful of models, plus a few hundred
 * entities) and serves valid RCMS buffers for it, so the viewport can be
 * developed, profiled and regression-checked without the Rust backend.
 *
 * Everything is deterministic (mulberry32 seeded from a constant), which is
 * what makes `MOCK_PROBE` a usable check of the row-major <-> three.js matrix
 * conversion.
 */

import * as THREE from "three";
import { encodeRcms, emptyRcms } from "./viewport/rcms";
import {
  KIND_ACTOR,
  KIND_DECAL,
  KIND_LIGHT,
  KIND_MODEL,
  KIND_SPLINE,
  KIND_VFX,
  kindName,
  type SceneObjectDto,
  type Vec3,
  type ZoneModelRef,
  type ZoneScene,
} from "./types";

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface MockModel {
  id: string;
  path: string;
  geometry: () => THREE.BufferGeometry;
  weight: number;
}

const MOCK_MODELS: MockModel[] = [
  { id: "0x8000000000000101", path: "models/mock/crate.model", weight: 30, geometry: () => new THREE.BoxGeometry(2, 2, 2) },
  { id: "0x8000000000000102", path: "models/mock/barrel.model", weight: 20, geometry: () => new THREE.CylinderGeometry(0.8, 0.8, 2.2, 16) },
  { id: "0x8000000000000103", path: "models/mock/rock.model", weight: 18, geometry: () => new THREE.IcosahedronGeometry(1.6, 1) },
  { id: "0x8000000000000104", path: "models/mock/pillar.model", weight: 12, geometry: () => new THREE.CylinderGeometry(1.1, 1.4, 9, 12) },
  { id: "0x8000000000000105", path: "models/mock/canopy.model", weight: 10, geometry: () => new THREE.ConeGeometry(4, 6, 10) },
  { id: "0x8000000000000106", path: "models/mock/arch.model", weight: 8, geometry: () => new THREE.TorusGeometry(4, 0.6, 8, 20, Math.PI) },
  // Deliberately undecodable: exercises the placeholder-box path.
  { id: "0x8000000000000107", path: "models/mock/broken.model", weight: 4, geometry: () => new THREE.BufferGeometry() },
];

const BROKEN_MODEL_ID = "0x8000000000000107";

/** Known instance used to verify the matrix convention end to end. */
export interface MockProbe {
  offset: number;
  position: Vec3;
  matrix: number[];
}

export interface MockScene {
  scene: ZoneScene;
  probe: MockProbe;
}

const OFFSET_BASE = 0x1000;
const OFFSET_STRIDE = 0x90;
const TILE = 512; // metres across, like a real openarea tile

/**
 * Build a mock zone. `instanceCount` model instances + `entityCount` entities.
 */
export function buildMockScene(instanceCount = 4200, entityCount = 320, seed = 0x5eed1234): MockScene {
  const rnd = mulberry32(seed);
  const objects: SceneObjectDto[] = [];
  const counts = new Map<string, number>();

  const pickModel = (): MockModel => {
    const total = MOCK_MODELS.reduce((a, m) => a + m.weight, 0);
    let r = rnd() * total;
    for (const m of MOCK_MODELS) {
      r -= m.weight;
      if (r <= 0) return m;
    }
    return MOCK_MODELS[0];
  };

  const m4 = new THREE.Matrix4();
  const q = new THREE.Quaternion();
  const e = new THREE.Euler();
  const pos = new THREE.Vector3();
  const scl = new THREE.Vector3();

  let offset = OFFSET_BASE;
  let probe: MockProbe | null = null;

  for (let i = 0; i < instanceCount; i++) {
    const model = pickModel();
    counts.set(model.id, (counts.get(model.id) ?? 0) + 1);

    const x = (rnd() - 0.5) * TILE;
    const z = (rnd() - 0.5) * TILE;
    // Gentle rolling terrain so the tile is not a flat plane.
    const y = Math.sin(x * 0.017) * 9 + Math.cos(z * 0.021) * 7 + rnd() * 1.5;

    e.set(0, rnd() * Math.PI * 2, 0);
    q.setFromEuler(e);
    const s = 0.6 + rnd() * 1.8;
    pos.set(x, y, z);
    scl.set(s, s, s);
    m4.compose(pos, q, scl);

    // Matrix4.toArray() yields column-major, which read as a flat array IS the
    // game's row-major layout (they are transposes of each other). Translation
    // therefore lands at indices 12,13,14 — exactly where the zone keeps it.
    const matrix = m4.toArray();

    const o: SceneObjectDto = {
      offset,
      kind: KIND_MODEL,
      kindName: kindName(KIND_MODEL),
      size: OFFSET_STRIDE,
      flags: 0,
      matrix,
      scale: [s, s, s],
      instanceId: `0x${(0x9000000000000000n + BigInt(i)).toString(16)}`,
      modelIndex: MOCK_MODELS.indexOf(model),
      modelId: model.id,
      name: "",
      actorIndex: -1,
    };
    objects.push(o);

    if (i === 0) probe = { offset, position: [x, y, z], matrix: matrix.slice() };
    offset += OFFSET_STRIDE;
  }

  const entityKinds = [KIND_LIGHT, KIND_ACTOR, KIND_DECAL, KIND_SPLINE, KIND_VFX];
  const actors: ZoneScene["actors"] = [];
  for (let i = 0; i < entityCount; i++) {
    const kind = entityKinds[i % entityKinds.length];
    const x = (rnd() - 0.5) * TILE;
    const z = (rnd() - 0.5) * TILE;
    const y = Math.sin(x * 0.017) * 9 + Math.cos(z * 0.021) * 7 + 2 + rnd() * 12;

    e.set(0, rnd() * Math.PI * 2, 0);
    q.setFromEuler(e);
    pos.set(x, y, z);
    scl.set(1, 1, 1);
    m4.compose(pos, q, scl);

    const extent: Vec3 = [0.8 + rnd() * 3, 0.8 + rnd() * 3, 0.8 + rnd() * 3];
    const isActor = kind === KIND_ACTOR;
    const name = isActor ? `actor_${i.toString().padStart(3, "0")}` : `${kindName(kind)}_${i}`;

    objects.push({
      offset,
      kind,
      kindName: kindName(kind),
      size: OFFSET_STRIDE,
      flags: kind << 24,
      matrix: m4.toArray(),
      scale: extent,
      instanceId: `0x${(0xa000000000000000n + BigInt(i)).toString(16)}`,
      modelIndex: -1,
      modelId: "0x0",
      name,
      actorIndex: isActor ? actors.length : -1,
    });

    if (isActor) {
      actors.push({
        index: actors.length,
        sceneOffset: offset,
        instanceId: `0x${(0xa000000000000000n + BigInt(i)).toString(16)}`,
        name,
        assetId: `0x${(0xb000000000000000n + BigInt(i)).toString(16)}`,
        assetPath: `actors/mock/${name}.actor`,
      });
    }
    offset += OFFSET_STRIDE;
  }

  const models: ZoneModelRef[] = MOCK_MODELS.filter((m) => counts.has(m.id))
    .map((m) => ({ id: m.id, path: m.path, instanceCount: counts.get(m.id)! }))
    .sort((a, b) => b.instanceCount - a.instanceCount);

  const min: Vec3 = [Infinity, Infinity, Infinity];
  const max: Vec3 = [-Infinity, -Infinity, -Infinity];
  for (const o of objects) {
    for (let i = 0; i < 3; i++) {
      const v = o.matrix[12 + i];
      if (v < min[i]) min[i] = v;
      if (v > max[i]) max[i] = v;
    }
  }

  const scene: ZoneScene = {
    path: "levels/i29/openarea/sargasso/tile_0_0/tile_0_0_art.zone",
    id: "0x8f3a1c02de44b910",
    sizes: [objects.length * OFFSET_STRIDE, 0, 0, 0],
    dirty: false,
    bounds: { min, max },
    models,
    objects,
    actors,
    sections: [
      { tag: "0x3E1A9C6B", name: "SceneObjects", size: objects.length * OFFSET_STRIDE },
      { tag: "0x2D80F111", name: "ModelIds", size: models.length * 8 },
      { tag: "0x6A00C4E2", name: "Actors", size: actors.length * 32 },
    ],
  };

  return { scene, probe: probe! };
}

/** RCMS buffers for the mock models, built lazily and cached. */
const bufferCache = new Map<string, ArrayBuffer>();

export function mockMeshBuffer(modelId: string): ArrayBuffer {
  const hit = bufferCache.get(modelId);
  if (hit) return hit;

  const model = MOCK_MODELS.find((m) => m.id === modelId);
  let buf: ArrayBuffer;
  if (!model || modelId === BROKEN_MODEL_ID) {
    buf = emptyRcms();
  } else {
    const geo = model.geometry();
    const indexed = geo.getIndex() ? geo : toIndexed(geo);
    indexed.computeVertexNormals();
    buf = encodeRcms(indexed);
    indexed.dispose();
    if (indexed !== geo) geo.dispose();
  }
  bufferCache.set(modelId, buf);
  return buf;
}

/** Mock `meshLoader`; `delayMs` simulates the IPC round trip. */
export function mockMeshLoader(delayMs = 220): (modelId: string) => Promise<ArrayBuffer> {
  return (modelId: string) =>
    new Promise((resolve) => {
      setTimeout(() => resolve(mockMeshBuffer(modelId)), delayMs);
    });
}

function toIndexed(geo: THREE.BufferGeometry): THREE.BufferGeometry {
  const pos = geo.getAttribute("position");
  if (!pos) return geo;
  const out = geo.clone();
  const idx = new Uint32Array(pos.count);
  for (let i = 0; i < pos.count; i++) idx[i] = i;
  out.setIndex(new THREE.BufferAttribute(idx, 1));
  return out;
}
