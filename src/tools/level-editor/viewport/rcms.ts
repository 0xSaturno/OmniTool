/**
 * RCMS — the raw binary mesh payload returned by `level_model_mesh`.
 *
 * Little-endian layout:
 *   u32  magic       = 0x534D4352 ("RCMS")
 *   u32  version     = 1
 *   u32  vertexCount
 *   u32  indexCount
 *   u32  submeshCount
 *   u32  reserved    = 0
 *   f32  bounds[6]   (minx,miny,minz, maxx,maxy,maxz)
 *   f32  positions[3 * vertexCount]
 *   f32  normals  [3 * vertexCount]
 *   f32  uvs      [2 * vertexCount]
 *   u32  indices  [indexCount]
 *   submeshCount * { u32 firstIndex, u32 indexCount, u32 materialIndex, u32 flags }
 *
 * A model that cannot be decoded comes back as a header-only buffer with zero
 * counts rather than an error, so `decodeRcms` returns `null` for it.
 */

import * as THREE from "three";

export const RCMS_MAGIC = 0x534d4352;
export const RCMS_VERSION = 1;
export const RCMS_HEADER_BYTES = 6 * 4 + 6 * 4; // 24 counts + 24 bounds = 48

export interface RcmsSubmesh {
  firstIndex: number;
  indexCount: number;
  materialIndex: number;
  flags: number;
}

export interface RcmsMesh {
  vertexCount: number;
  indexCount: number;
  positions: Float32Array;
  normals: Float32Array;
  uvs: Float32Array;
  indices: Uint32Array;
  submeshes: RcmsSubmesh[];
  min: [number, number, number];
  max: [number, number, number];
}

/**
 * Decode an RCMS buffer. Returns `null` for an empty/placeholder buffer or for
 * anything that fails validation — callers keep their placeholder geometry.
 */
export function decodeRcms(buf: ArrayBuffer): RcmsMesh | null {
  if (!buf || buf.byteLength < RCMS_HEADER_BYTES) return null;

  const dv = new DataView(buf);
  if (dv.getUint32(0, true) !== RCMS_MAGIC) return null;
  if (dv.getUint32(4, true) !== RCMS_VERSION) return null;

  const vertexCount = dv.getUint32(8, true);
  const indexCount = dv.getUint32(12, true);
  const submeshCount = dv.getUint32(16, true);
  if (vertexCount === 0 || indexCount === 0) return null;

  const min: [number, number, number] = [dv.getFloat32(24, true), dv.getFloat32(28, true), dv.getFloat32(32, true)];
  const max: [number, number, number] = [dv.getFloat32(36, true), dv.getFloat32(40, true), dv.getFloat32(44, true)];

  const posBytes = vertexCount * 3 * 4;
  const nrmBytes = vertexCount * 3 * 4;
  const uvBytes = vertexCount * 2 * 4;
  const idxBytes = indexCount * 4;
  const subBytes = submeshCount * 16;
  const needed = RCMS_HEADER_BYTES + posBytes + nrmBytes + uvBytes + idxBytes + subBytes;
  if (buf.byteLength < needed) return null;

  let o = RCMS_HEADER_BYTES;
  // Typed-array views over the buffer need natural alignment; the header is a
  // multiple of 4 and every block is 4-byte sized, so slicing is safe here, but
  // copy anyway so the geometry owns its memory and the source can be released.
  const positions = new Float32Array(buf.slice(o, o + posBytes));
  o += posBytes;
  const normals = new Float32Array(buf.slice(o, o + nrmBytes));
  o += nrmBytes;
  const uvs = new Float32Array(buf.slice(o, o + uvBytes));
  o += uvBytes;
  const indices = new Uint32Array(buf.slice(o, o + idxBytes));
  o += idxBytes;

  const submeshes: RcmsSubmesh[] = [];
  for (let i = 0; i < submeshCount; i++) {
    const b = o + i * 16;
    submeshes.push({
      firstIndex: dv.getUint32(b, true),
      indexCount: dv.getUint32(b + 4, true),
      materialIndex: dv.getUint32(b + 8, true),
      flags: dv.getUint32(b + 12, true),
    });
  }

  return { vertexCount, indexCount, positions, normals, uvs, indices, submeshes, min, max };
}

/**
 * Fraction of sampled triangles whose winding agrees with the supplied vertex
 * normals — i.e. `cross(b-a, c-a)` (the three.js / glTF front-face direction)
 * points the same way as the averaged vertex normal.
 *
 * Returns -1 when nothing could be sampled (degenerate or normal-less mesh).
 */
export function windingAgreement(
  positions: Float32Array,
  normals: Float32Array,
  indices: Uint32Array,
  sampleLimit = 4096,
): number {
  const vertexCount = positions.length / 3;
  const triCount = (indices.length / 3) | 0;
  if (triCount === 0 || normals.length < vertexCount * 3) return -1;
  const step = Math.max(1, Math.ceil(triCount / sampleLimit));

  let agree = 0;
  let total = 0;
  for (let t = 0; t < triCount; t += step) {
    const i0 = indices[t * 3] * 3;
    const i1 = indices[t * 3 + 1] * 3;
    const i2 = indices[t * 3 + 2] * 3;
    if (i0 + 2 >= positions.length || i1 + 2 >= positions.length || i2 + 2 >= positions.length) continue;

    const ax = positions[i1] - positions[i0];
    const ay = positions[i1 + 1] - positions[i0 + 1];
    const az = positions[i1 + 2] - positions[i0 + 2];
    const bx = positions[i2] - positions[i0];
    const by = positions[i2 + 1] - positions[i0 + 1];
    const bz = positions[i2 + 2] - positions[i0 + 2];

    const fx = ay * bz - az * by;
    const fy = az * bx - ax * bz;
    const fz = ax * by - ay * bx;
    if (fx * fx + fy * fy + fz * fz < 1e-24) continue; // degenerate triangle

    const nx = normals[i0] + normals[i1] + normals[i2];
    const ny = normals[i0 + 1] + normals[i1 + 1] + normals[i2 + 1];
    const nz = normals[i0 + 2] + normals[i1 + 2] + normals[i2 + 2];
    if (nx * nx + ny * ny + nz * nz < 1e-12) continue; // normals cancel out

    total++;
    if (fx * nx + fy * ny + fz * nz > 0) agree++;
  }
  return total === 0 ? -1 : agree / total;
}

/** Below this agreement a mesh is reported as mis-wound. */
export const WINDING_SUSPECT_THRESHOLD = 0.4;

/** Dev builds measure winding and complain; production skips the sampling. */
const CHECK_WINDING = import.meta.env.DEV;

/** Build a BufferGeometry from a decoded RCMS mesh. */
export function rcmsToGeometry(mesh: RcmsMesh): THREE.BufferGeometry {
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(mesh.positions, 3));

  let hasNormals = false;
  for (let i = 0; i < mesh.normals.length; i += 3) {
    if (mesh.normals[i] !== 0 || mesh.normals[i + 1] !== 0 || mesh.normals[i + 2] !== 0) {
      hasNormals = true;
      break;
    }
  }

  // WINDING. This used to reverse the index buffer of any mesh whose triangles
  // disagreed with its own vertex normals, because `decode_model_mesh` emitted
  // every triangle backwards and three.js back-face culled most of the level.
  // The decoder now emits source order and the workaround is gone: measured
  // over the meshes of two real zones after that fix, 414 of 414 models agree
  // with their normals on more than 90% of sampled triangles (savali
  // `tile_c17_art`, 79 models, min 0.893 / mean 0.995; nefarious
  // `tile_e19_art`, 335 models, min 0.983 / mean 0.9998), and none is below
  // 10%. Block-2 geometry agrees too: all 91 savali decals above 0.999, and
  // impostors are 1.0 by construction since their normals are accumulated from
  // the very triangles the decoder emits.
  //
  // Keeping the flip would now be actively wrong rather than merely dead. 13 of
  // nefarious `tile_e19_art`'s 218 decals measure exactly 0.0 agreement — tiny
  // sheets, 604 triangles between them (0.29% of that zone's decal geometry,
  // largest 196 triangles), whose authored normals face the projector rather
  // than the surface. That is data, not a decode error; they are drawn
  // double-sided so it changes nothing on screen, but the old heuristic would
  // have silently rewritten their index buffers and reported 13 "rewound"
  // meshes that were never wrong.
  //
  // What survives is a dev-only assertion. It costs 6.8 ms over savali's 170
  // meshes / 885k unique triangles, runs only under `import.meta.env.DEV`, and
  // never mutates anything: if the decoder regresses, the console says so and
  // the HUD counts it, instead of the pipeline quietly cancelling the bug.
  let agreement = -1;
  if (hasNormals && CHECK_WINDING) {
    agreement = windingAgreement(mesh.positions, mesh.normals, mesh.indices);
    if (agreement >= 0 && agreement < WINDING_SUSPECT_THRESHOLD) {
      console.warn(
        `[rcms] mesh winding disagrees with its normals on ${((1 - agreement) * 100).toFixed(1)}% of ` +
          `sampled triangles (${mesh.indexCount / 3} tris). Geometry is drawn as decoded.`,
      );
    }
  }

  if (hasNormals) g.setAttribute("normal", new THREE.BufferAttribute(mesh.normals, 3));
  if (mesh.uvs.length === mesh.vertexCount * 2) g.setAttribute("uv", new THREE.BufferAttribute(mesh.uvs, 2));

  g.setIndex(new THREE.BufferAttribute(mesh.indices, 1));
  if (!hasNormals) g.computeVertexNormals();

  g.userData.windingAgreement = agreement;
  g.userData.windingSuspect = agreement >= 0 && agreement < WINDING_SUSPECT_THRESHOLD;

  const finite =
    Number.isFinite(mesh.min[0]) && Number.isFinite(mesh.max[0]) && mesh.max[0] >= mesh.min[0];
  if (finite) {
    g.boundingBox = new THREE.Box3(
      new THREE.Vector3(mesh.min[0], mesh.min[1], mesh.min[2]),
      new THREE.Vector3(mesh.max[0], mesh.max[1], mesh.max[2]),
    );
    g.boundingSphere = g.boundingBox.getBoundingSphere(new THREE.Sphere());
  } else {
    g.computeBoundingBox();
    g.computeBoundingSphere();
  }
  return g;
}

/** Serialise a BufferGeometry back into an RCMS buffer. */
export function encodeRcms(geometry: THREE.BufferGeometry): ArrayBuffer {
  const pos = geometry.getAttribute("position") as THREE.BufferAttribute | undefined;
  const nrm = geometry.getAttribute("normal") as THREE.BufferAttribute | undefined;
  const uv = geometry.getAttribute("uv") as THREE.BufferAttribute | undefined;
  const idx = geometry.getIndex();
  if (!pos || !idx) return emptyRcms();

  const vertexCount = pos.count;
  const indexCount = idx.count;
  const submeshCount = 1;

  const bytes =
    RCMS_HEADER_BYTES + vertexCount * 3 * 4 * 2 + vertexCount * 2 * 4 + indexCount * 4 + submeshCount * 16;
  const buf = new ArrayBuffer(bytes);
  const dv = new DataView(buf);

  geometry.computeBoundingBox();
  const bb = geometry.boundingBox!;

  dv.setUint32(0, RCMS_MAGIC, true);
  dv.setUint32(4, RCMS_VERSION, true);
  dv.setUint32(8, vertexCount, true);
  dv.setUint32(12, indexCount, true);
  dv.setUint32(16, submeshCount, true);
  dv.setUint32(20, 0, true);
  dv.setFloat32(24, bb.min.x, true);
  dv.setFloat32(28, bb.min.y, true);
  dv.setFloat32(32, bb.min.z, true);
  dv.setFloat32(36, bb.max.x, true);
  dv.setFloat32(40, bb.max.y, true);
  dv.setFloat32(44, bb.max.z, true);

  let o = RCMS_HEADER_BYTES;
  const fPos = new Float32Array(buf, o, vertexCount * 3);
  for (let i = 0; i < vertexCount; i++) {
    fPos[i * 3] = pos.getX(i);
    fPos[i * 3 + 1] = pos.getY(i);
    fPos[i * 3 + 2] = pos.getZ(i);
  }
  o += vertexCount * 3 * 4;

  const fNrm = new Float32Array(buf, o, vertexCount * 3);
  if (nrm) {
    for (let i = 0; i < vertexCount; i++) {
      fNrm[i * 3] = nrm.getX(i);
      fNrm[i * 3 + 1] = nrm.getY(i);
      fNrm[i * 3 + 2] = nrm.getZ(i);
    }
  }
  o += vertexCount * 3 * 4;

  const fUv = new Float32Array(buf, o, vertexCount * 2);
  if (uv) {
    for (let i = 0; i < vertexCount; i++) {
      fUv[i * 2] = uv.getX(i);
      fUv[i * 2 + 1] = uv.getY(i);
    }
  }
  o += vertexCount * 2 * 4;

  const uIdx = new Uint32Array(buf, o, indexCount);
  for (let i = 0; i < indexCount; i++) uIdx[i] = idx.getX(i);
  o += indexCount * 4;

  dv.setUint32(o, 0, true);
  dv.setUint32(o + 4, indexCount, true);
  dv.setUint32(o + 8, 0, true);
  dv.setUint32(o + 12, 0, true);

  return buf;
}

/** A header-only RCMS buffer — what the backend returns for an undecodable model. */
export function emptyRcms(): ArrayBuffer {
  const buf = new ArrayBuffer(RCMS_HEADER_BYTES);
  const dv = new DataView(buf);
  dv.setUint32(0, RCMS_MAGIC, true);
  dv.setUint32(4, RCMS_VERSION, true);
  return buf;
}
