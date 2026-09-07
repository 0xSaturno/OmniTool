/**
 * Zone <-> three.js transform conversion.
 *
 * The zone stores a 4x4 matrix as 16 consecutive floats in ROW-MAJOR order with
 * the translation in row 3 (flat indices 12,13,14) — the row-vector convention
 * (v' = v * M) used by the game.
 *
 * three.js uses the column-vector convention (v' = M * v) and stores
 * `Matrix4.elements` in COLUMN-MAJOR order, with the translation at elements
 * 12,13,14. The three matrix we want is therefore the TRANSPOSE of the zone
 * matrix — and reading a row-major array as column-major *is* a transpose.
 *
 * So the conversion collapses to:
 *     zone -> three :  m.fromArray(zoneMatrix)
 *     three -> zone :  m.toArray()
 *
 * Sanity check on the translation: zoneMatrix[12..14] is (tx,ty,tz);
 * `fromArray` copies it straight into elements[12..14], which is exactly where
 * three keeps the translation. Verified against mock instance #0 in the dev
 * harness (mock position and `InstancedMesh.getMatrixAt().decompose()` agree to
 * float precision).
 */

import * as THREE from "three";

/** Zone row-major float[16] -> three.js Matrix4. */
export function zoneToMatrix4(zone: ArrayLike<number>, out?: THREE.Matrix4): THREE.Matrix4 {
  const m = out ?? new THREE.Matrix4();
  return m.fromArray(zone as number[]);
}

/** three.js Matrix4 -> zone row-major float[16]. */
export function matrix4ToZone(m: THREE.Matrix4): number[] {
  return m.toArray();
}

/** Translation of a zone matrix, without building a Matrix4. */
export function zoneTranslation(zone: ArrayLike<number>, out?: THREE.Vector3): THREE.Vector3 {
  const v = out ?? new THREE.Vector3();
  return v.set(zone[12], zone[13], zone[14]);
}

/** True when every component is finite (guards against garbage in stub zones). */
export function isFiniteMatrix(zone: ArrayLike<number>): boolean {
  if (zone.length < 16) return false;
  for (let i = 0; i < 16; i++) if (!Number.isFinite(zone[i])) return false;
  return true;
}
