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
  if (hasNormals) g.setAttribute("normal", new THREE.BufferAttribute(mesh.normals, 3));
  if (mesh.uvs.length === mesh.vertexCount * 2) g.setAttribute("uv", new THREE.BufferAttribute(mesh.uvs, 2));

  g.setIndex(new THREE.BufferAttribute(mesh.indices, 1));
  if (!hasNormals) g.computeVertexNormals();

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

/** Serialise a BufferGeometry back into an RCMS buffer (used by the mock source). */
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
