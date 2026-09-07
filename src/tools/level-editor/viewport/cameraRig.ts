/**
 * Camera rig for the level viewport.
 *
 * Orbit / pan / dolly around a target point, plus a WASD+QE fly mode that moves
 * the target with the camera so orbiting stays sane afterwards. Written by hand
 * instead of using OrbitControls so that fly, framing and the "did the camera
 * move" signal (used to drive on-demand rendering) all live in one place.
 *
 * Mouse map (matches the other DCC-style tools):
 *   LMB drag  orbit
 *   MMB drag  pan
 *   RMB drag  pan
 *   wheel     dolly toward / away from the target
 *   LMB click (no drag) is left to the caller for picking.
 */

import * as THREE from "three";

const MIN_DISTANCE = 0.25;
const MAX_DISTANCE = 60000;
const EPS = 1e-4;

export class CameraRig {
  readonly camera: THREE.PerspectiveCamera;
  readonly target = new THREE.Vector3();

  /** Base fly speed in m/s; further scaled by how far out the camera is. */
  flySpeed = 8;

  private spherical = new THREE.Spherical(50, Math.PI / 3, Math.PI / 4);
  private keys = new Set<string>();
  private dirty = true;

  constructor(camera: THREE.PerspectiveCamera) {
    this.camera = camera;
    this.applySpherical();
  }

  // -- state ----------------------------------------------------------------

  /** Consume-and-clear the "camera changed" flag. */
  takeDirty(): boolean {
    const d = this.dirty;
    this.dirty = false;
    return d;
  }

  // -- mouse ----------------------------------------------------------------

  orbit(dxPx: number, dyPx: number): void {
    this.spherical.theta -= dxPx * 0.006;
    this.spherical.phi -= dyPx * 0.006;
    this.spherical.phi = Math.max(EPS, Math.min(Math.PI - EPS, this.spherical.phi));
    this.applySpherical();
  }

  pan(dxPx: number, dyPx: number, viewportHeight: number): void {
    // Screen-space pan scaled so a pixel maps to the same world distance at the
    // target plane regardless of fov or distance.
    const fov = (this.camera.fov * Math.PI) / 180;
    const worldPerPixel = (2 * Math.tan(fov / 2) * this.spherical.radius) / Math.max(1, viewportHeight);

    const right = new THREE.Vector3().setFromMatrixColumn(this.camera.matrix, 0);
    const up = new THREE.Vector3().setFromMatrixColumn(this.camera.matrix, 1);

    const move = new THREE.Vector3()
      .addScaledVector(right, -dxPx * worldPerPixel)
      .addScaledVector(up, dyPx * worldPerPixel);

    this.target.add(move);
    this.camera.position.add(move);
    this.dirty = true;
  }

  dolly(wheelDeltaY: number): void {
    const factor = Math.exp(wheelDeltaY * 0.0012);
    this.spherical.radius = THREE.MathUtils.clamp(this.spherical.radius * factor, MIN_DISTANCE, MAX_DISTANCE);
    this.applySpherical();
  }

  // -- keyboard fly ---------------------------------------------------------

  setKey(code: string, down: boolean): void {
    if (down) this.keys.add(code);
    else this.keys.delete(code);
  }

  clearKeys(): void {
    this.keys.clear();
  }

  get flying(): boolean {
    for (const k of ["KeyW", "KeyA", "KeyS", "KeyD", "KeyQ", "KeyE"]) if (this.keys.has(k)) return true;
    return false;
  }

  /** Advance the fly movement; `dt` in seconds. Returns true if anything moved. */
  update(dt: number): boolean {
    if (!this.flying) return false;

    const fast = this.keys.has("ShiftLeft") || this.keys.has("ShiftRight");
    const slow = this.keys.has("AltLeft") || this.keys.has("AltRight");
    let speed = this.flySpeed * (fast ? 6 : 1) * (slow ? 0.2 : 1);
    // Scale with how far out we are so flying across a whole planet is usable.
    speed *= THREE.MathUtils.clamp(this.spherical.radius / 50, 0.2, 40);

    const forward = new THREE.Vector3();
    this.camera.getWorldDirection(forward);
    const right = new THREE.Vector3().setFromMatrixColumn(this.camera.matrix, 0);

    const move = new THREE.Vector3();
    if (this.keys.has("KeyW")) move.addScaledVector(forward, 1);
    if (this.keys.has("KeyS")) move.addScaledVector(forward, -1);
    if (this.keys.has("KeyD")) move.addScaledVector(right, 1);
    if (this.keys.has("KeyA")) move.addScaledVector(right, -1);
    if (this.keys.has("KeyE")) move.y += 1;
    if (this.keys.has("KeyQ")) move.y -= 1;

    if (move.lengthSq() === 0) return false;
    move.normalize().multiplyScalar(speed * dt);

    this.camera.position.add(move);
    this.target.add(move);
    this.dirty = true;
    return true;
  }

  // -- framing --------------------------------------------------------------

  /** Point the camera at a box, keeping the current orbit angles. */
  frameBox(box: THREE.Box3, fill = 1.4): void {
    if (box.isEmpty()) return;
    const center = box.getCenter(new THREE.Vector3());
    const size = box.getSize(new THREE.Vector3());
    const radius = Math.max(size.x, size.y, size.z, 0.5) * 0.5;

    const fov = (this.camera.fov * Math.PI) / 180;
    const dist = THREE.MathUtils.clamp((radius / Math.sin(fov / 2)) * fill, MIN_DISTANCE, MAX_DISTANCE);

    this.target.copy(center);
    this.spherical.radius = dist;
    this.applySpherical();
  }

  /** Frame a box from a fresh three-quarter viewpoint. */
  frameBoxDefaultAngle(box: THREE.Box3, fill = 1.4): void {
    this.spherical.theta = Math.PI * 0.25;
    this.spherical.phi = Math.PI * 0.33;
    this.frameBox(box, fill);
  }

  private applySpherical(): void {
    const offset = new THREE.Vector3().setFromSpherical(this.spherical);
    this.camera.position.copy(this.target).add(offset);
    this.camera.lookAt(this.target);
    this.camera.updateMatrixWorld();
    this.dirty = true;
  }
}
