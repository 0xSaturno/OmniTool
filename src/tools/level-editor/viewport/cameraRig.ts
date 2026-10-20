/**
 * Camera rig for the level viewport.
 *
 * Orbit / pan / dolly around a pivot, plus a WASD+QE fly mode that carries the
 * pivot along so orbiting stays sane afterwards. Written by hand instead of
 * using OrbitControls so that fly, framing, the surface probe and the "did the
 * camera move" signal (which drives on-demand rendering) all live in one place.
 *
 * WHY THE PROBE. Every rate here — fly speed, wheel step, pan — scales with the
 * distance to the SURFACE the camera is pointed at, not with the distance to
 * the pivot. The pivot is wherever the last frame-selection left it, which is a
 * terrible proxy for "how much room is in front of me": fly up to a building
 * while the pivot is still the tile centre and a radius-scaled speed stays at
 * tens of metres per second a metre from the wall, while orbiting in tight
 * makes flying across the same tile crawl. `setSurfaceProbe` hands the rig a
 * raycast; it is cached and rate-limited here, and between probes the cached
 * hit is carried forward by how far the camera has travelled along the probe
 * ray, so the number stays live at no cost.
 *
 * The pivot is kept ON the view axis by every motion below (orbit rotates about
 * it, pan and fly translate both, the wheel scales both about the point under
 * the cursor, which preserves the camera->pivot direction). That invariant is
 * what lets `recenterPivot` / `setPivotAtPoint` re-anchor the pivot to a probed
 * surface without rotating or moving the camera a millimetre.
 *
 * Mouse map (matches the other DCC-style tools):
 *   LMB drag  orbit, around the surface in the middle of the screen
 *   MMB drag  pan
 *   RMB drag  pan
 *   wheel     dolly toward the cursor, easing in as the surface gets closer
 *   LMB click (no drag) is left to the caller for picking.
 */

import * as THREE from "three";

/**
 * Closest the pivot may sit to the camera. 0.1 m is the renderer's near plane,
 * so the pivot can reach the nearest thing that is still drawn; the old 0.25 m
 * also outranked the wheel's own stop distance, which froze the wheel 50 mm
 * short of where it meant to park.
 */
const MIN_DISTANCE = 0.1;
const MAX_DISTANCE = 60000;
const EPS = 1e-4;

/**
 * Seconds to cross the probed distance at the base fly speed: speed is
 * `surface / SECONDS_TO_SURFACE`, which is scale-invariant — five seconds to
 * reach what you are looking at whether it is 3 m or 3 km away.
 */
const SECONDS_TO_SURFACE = 5;
/** m/s floor: slow enough to park against a wall, fast enough to push through. */
const MIN_FLY_SPEED = 1.2;
/** m/s ceiling, ~4 s to cross the widest shipped region. */
const MAX_FLY_SPEED = 1500;
/**
 * Reference distance floor when the probe sees nothing (camera pointed at sky).
 * Without it, a camera that had orbited in to a 0.4 m radius would still crawl
 * while flying away into open space.
 */
const MIN_MISS_REFERENCE = 20;
/** Slowing down is near-immediate: 1/e of the gap closed in ~0.08 s. */
const SPEED_DOWN_RATE = 12;
/** Speeding up is capped at x4 per second, so no probe result is a teleport. */
const SPEED_UP_LN = Math.log(4);

/**
 * How long a real hit is remembered, and how much more room a MISS may claim
 * while a CLOSE hit is still remembered.
 *
 * A miss can mean open sky, but it can also mean the camera is INSIDE a
 * building: three.js honours `material.side`, so from in there every wall the
 * ray would hit is back-facing and the raycast finds nothing. Measured on
 * savali tile_c17_art: pushing through a shell at the 1.2 m/s floor made the
 * next probe miss, the fallback took the 1,019 m orbit radius, and the camera
 * left the room at 204 m/s. A wall a metre away a moment ago means the camera
 * is in a tight place whichever way it now points, so until that memory ages
 * out a miss is worth a few times that distance and no more; the x4-per-second
 * growth cap then does the rest of the work. It is belt and braces now that the
 * probe raycasts a double-sided stand-in and does see walls from inside, so the
 * memory is short: backing out of a tight spot should not stay slow for long.
 */
const MISS_MEMORY_MS = 800;
const MISS_HIT_FACTOR = 4;
/** A hit this close counts as "working inside a structure" for the rule above. */
const MISS_NEAR_METRES = 25;

/** Wheel: fraction of the distance to the cursor surface per notch = 1-e^-0.2. */
const WHEEL_RATE = 0.002;
/**
 * The wheel may not park the camera closer than this to the surface it zooms
 * at: it eases in asymptotically and stops here, which is still outside the
 * 0.1 m near plane, so the last frame before the stop is not clipped. Pushing
 * through geometry is for the fly keys, which do it deliberately.
 */
const MIN_SURFACE_GAP = 0.15;

/**
 * How far ahead a probe looks: `PROBE_WINDOW_FACTOR` times the reference it is
 * refining, floored at `PROBE_MIN_WINDOW`. The speed can only grow x4 per
 * second (SPEED_UP_LN), so looking four references ahead is already more room
 * than the next second can consume; a miss means "at least this much space",
 * and the window widens by the same factor on each probe that finds nothing,
 * so flying into open sky reaches the ceiling in about half a second.
 */
const PROBE_WINDOW_FACTOR = 4;
const PROBE_MIN_WINDOW = 50;
const PROBE_MAX_WINDOW = 20000;

/** Cached probe older than this is re-run before being trusted for a new frame. */
const PROBE_MAX_AGE_MS = 120;
/** ...but never re-run more often than this, whatever asks for it. */
const PROBE_MIN_INTERVAL_MS = 45;
/** Cached probe is reusable while the query direction is within ~3 degrees. */
const PROBE_DIR_TOLERANCE = Math.cos((3 * Math.PI) / 180);

/**
 * Distance from `origin` along the unit vector `dir` to the first surface
 * within `maxDist`, or null for "nothing that close". Supplied by the renderer,
 * which is the only thing that knows what is drawn.
 *
 * `maxDist` is not a convenience: it is what keeps the probe affordable. A full
 * unbounded raycast against savali tile_c17_art (9,917 instances, 91 block-2
 * meshes, 346,820 triangles) measured 14.1 ms, because every mesh whose
 * bounding sphere the ray crosses walks its whole triangle list. Bounding the
 * ray to the distance the speed could actually use lets the renderer reject
 * batches and triangles wholesale.
 */
export type SurfaceProbe = (origin: THREE.Vector3, dir: THREE.Vector3, maxDist: number) => number | null;

/** What the HUD shows about navigation. Read-only snapshot, never probes. */
export interface NavInfo {
  /** Metres to the surface ahead, carried forward from the cached probe. */
  surface: number | null;
  /** Metres per second the current fly keys would move at right now. */
  speed: number;
  /** Mean cost of one surface probe, milliseconds. */
  probeMs: number;
}

const _dir = new THREE.Vector3();
const _fwd = new THREE.Vector3();
const _probeDir = new THREE.Vector3();
const _anchor = new THREE.Vector3();
const _offset = new THREE.Vector3();
const _right = new THREE.Vector3();
const _up = new THREE.Vector3();
const _move = new THREE.Vector3();

export class CameraRig {
  readonly camera: THREE.PerspectiveCamera;
  readonly target = new THREE.Vector3();

  /** Multiplier on the probe-derived fly speed; 1 = `surface / 5` m/s. */
  flySpeed = 1;

  private spherical = new THREE.Spherical(50, Math.PI / 3, Math.PI / 4);
  private keys = new Set<string>();
  private dirty = true;

  // -- surface probe cache --
  private probe: SurfaceProbe | null = null;
  private probeOrigin = new THREE.Vector3();
  private probeRay = new THREE.Vector3(0, 0, -1);
  private probeHit: number | null = null;
  private probeTime = -Infinity;
  private probeValid = false;
  /** Rolling mean cost of one probe, milliseconds; shown in the HUD. */
  private probeMeanMs = 0;
  /** Did the last `surfaceDistance` call actually raycast? */
  private probeFresh = false;
  /** How far ahead the next probe looks; see PROBE_WINDOW_FACTOR. */
  private window = PROBE_MAX_WINDOW;
  /** The window the last probe actually used; a miss means at least this much room. */
  private windowUsed = PROBE_MAX_WINDOW;

  // -- last real hit, for the miss cap (see MISS_MEMORY_MS) --
  private lastHit = Infinity;
  private lastHitTime = -Infinity;

  private smoothedSpeed = 0;

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

  /** Distance from the camera to the pivot. */
  get radius(): number {
    return this.spherical.radius;
  }

  // -- surface probe --------------------------------------------------------

  /** Install (or clear) the raycast the rig scales its rates with. */
  setSurfaceProbe(probe: SurfaceProbe | null): void {
    this.probe = probe;
    this.invalidateSurface();
  }

  /**
   * Drop the cached hit. The renderer calls this when the scene changes under
   * the camera (zone swap, a streamed mesh replacing a placeholder box), since
   * a cached distance to geometry that no longer exists would scale the next
   * frame's speed off a ghost.
   *
   * This also re-arms the rate limiter: an invalidated cache means the next
   * query MUST measure. Leaving the limiter armed cost a measured 45 ms window
   * in which the rig fell back to the orbit radius — at a metre from a wall
   * with the pivot a kilometre out that is a 200 m/s speed spike, which is the
   * exact failure this whole change is about.
   */
  invalidateSurface(): void {
    this.probeValid = false;
    this.probeHit = null;
    this.probeTime = -Infinity;
  }

  /** True when a probe has run and its answer is still being carried forward. */
  get surfaceKnown(): boolean {
    return this.probeValid;
  }

  /**
   * Re-run the probe down the view axis if the cache is stale. Called from the
   * renderer's HUD tick so the readout is live and the cache is warm for the
   * next orbit or wheel event; the rate limiter below means a caller cannot
   * make this expensive by asking often.
   */
  refreshSurfaceProbe(): void {
    this.camera.getWorldDirection(_fwd);
    this.reference(_fwd);
  }

  /** Cached probe only — never raycasts. For the HUD. */
  navInfo(): NavInfo {
    const surface = this.peekSurface();
    const reference = surface ?? Math.max(this.spherical.radius, MIN_MISS_REFERENCE);
    return {
      surface,
      speed: this.baseSpeed(reference) * this.keyModifier(),
      probeMs: this.probeMeanMs,
    };
  }

  /**
   * Metres to the first surface along `dir`, or null.
   *
   * Cached per direction: a hit is carried forward by the camera's travel along
   * the stored ray, so between probes the number still shrinks as you approach
   * and goes to zero exactly as you reach the surface. A query more than
   * PROBE_DIR_TOLERANCE off the cached ray re-probes, unless the rate limiter
   * says no — in which case the carried-forward value is returned, which for
   * the few tens of milliseconds involved is a near-enough reference for a
   * speed and is never larger than the truth by more than the camera moved.
   */
  private surfaceDistance(dir: THREE.Vector3, maxDist = PROBE_MAX_WINDOW): number | null {
    this.probeFresh = false;
    if (!this.probe) return null;
    const now = performance.now();
    const aligned = this.probeValid && this.probeRay.dot(dir) >= PROBE_DIR_TOLERANCE;
    const stale = !aligned || now - this.probeTime >= PROBE_MAX_AGE_MS;

    if (stale && now - this.probeTime >= PROBE_MIN_INTERVAL_MS) {
      this.probeFresh = true;
      _probeDir.copy(dir).normalize();
      const t0 = performance.now();
      const hit = this.probe(this.camera.position, _probeDir, maxDist);
      const cost = performance.now() - t0;
      this.probeMeanMs = this.probeMeanMs === 0 ? cost : this.probeMeanMs * 0.8 + cost * 0.2;
      this.probeOrigin.copy(this.camera.position);
      this.probeRay.copy(_probeDir);
      this.probeHit = hit !== null && Number.isFinite(hit) ? hit : null;
      this.probeTime = now;
      this.probeValid = true;
      return this.probeHit;
    }

    return this.peekSurface();
  }

  /** Carried-forward cached hit; null when there is no usable cache. */
  private peekSurface(): number | null {
    if (!this.probeValid || this.probeHit === null) return null;
    const travelled = _offset.copy(this.camera.position).sub(this.probeOrigin).dot(this.probeRay);
    return Math.max(0, this.probeHit - travelled);
  }

  /**
   * The distance every rate scales with, given a probe along `dir`.
   *
   * The probe only looks `window` metres ahead (see PROBE_WINDOW_FACTOR). With
   * no usable cache the window is seeded from the orbit radius, which is the
   * "camera is pointed at sky" fallback the pivot is still good for; after that
   * it tracks the last measurement, and widens on every probe that finds
   * nothing so that open space opens up over a few probes instead of at once.
   */
  private reference(dir: THREE.Vector3): number {
    const surface = this.probeAhead(dir);
    if (surface !== null) return surface;
    const window = this.windowUsed;
    if (performance.now() - this.lastHitTime < MISS_MEMORY_MS && this.lastHit < MISS_NEAR_METRES) {
      // See MISS_MEMORY_MS. A wall a metre away a moment ago means the camera
      // is in a tight place, whichever way it now points, so a miss buys at
      // most a few times that much room - never the kilometre the pivot is at.
      return Math.min(window, Math.max(this.lastHit * MISS_HIT_FACTOR, MIN_FLY_SPEED * SECONDS_TO_SURFACE));
    }
    // Nothing near, nothing within the window: the camera is looking at sky,
    // and the pivot distance is the only scale cue left. The window widens on
    // each miss, so this ramps up to the pivot-based speed over a few probes
    // rather than jumping there.
    return Math.min(window, Math.max(this.spherical.radius, MIN_MISS_REFERENCE));
  }

  /**
   * Windowed probe: metres to the surface along `dir`, or null when there is
   * nothing within the current window — which means "at least `windowUsed`
   * metres of room", not "open sky". Maintains the window.
   */
  private probeAhead(dir: THREE.Vector3): number | null {
    if (!this.probeValid) {
      const seed = Math.max(this.spherical.radius, MIN_MISS_REFERENCE);
      this.window = THREE.MathUtils.clamp(seed * PROBE_WINDOW_FACTOR, PROBE_MIN_WINDOW, PROBE_MAX_WINDOW);
    }
    this.windowUsed = this.window;
    const hit = this.surfaceDistance(dir, this.windowUsed);
    if (hit !== null) {
      this.window = THREE.MathUtils.clamp(hit * PROBE_WINDOW_FACTOR, PROBE_MIN_WINDOW, PROBE_MAX_WINDOW);
      this.lastHit = hit;
      this.lastHitTime = performance.now();
      return hit;
    }
    // Widen only when a probe actually ran, or a cached miss would ramp the
    // speed up once per frame instead of once per measurement.
    if (this.probeFresh) this.window = Math.min(this.windowUsed * PROBE_WINDOW_FACTOR, PROBE_MAX_WINDOW);
    return null;
  }

  /** Probe-derived speed, before the shift/alt modifiers. This is what is smoothed. */
  private baseSpeed(reference: number): number {
    return THREE.MathUtils.clamp(reference / SECONDS_TO_SURFACE, MIN_FLY_SPEED, MAX_FLY_SPEED) * this.flySpeed;
  }

  /**
   * Shift / alt, applied AFTER the smoothing: holding shift is a decision, not
   * a measurement, so it has no business ramping through the growth cap.
   */
  private keyModifier(): number {
    const fast = this.keys.has("ShiftLeft") || this.keys.has("ShiftRight");
    const slow = this.keys.has("AltLeft") || this.keys.has("AltRight");
    return (fast ? 6 : 1) * (slow ? 0.2 : 1);
  }

  /** Unit world direction of a cursor position in NDC, or the view axis. */
  private cursorDirection(ndc: THREE.Vector2 | null | undefined, out: THREE.Vector3): THREE.Vector3 {
    if (!ndc) {
      this.camera.getWorldDirection(out);
      return out;
    }
    return out.set(ndc.x, ndc.y, 0.5).unproject(this.camera).sub(this.camera.position).normalize();
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
    // plane being dragged, regardless of fov or distance. That plane is the
    // probed surface when it is nearer than the pivot: a metre from a wall with
    // the pivot 500 m back, scaling by the radius made one pixel a quarter of a
    // metre and threw the view off the building.
    this.camera.getWorldDirection(_fwd);
    const surface = this.probeAhead(_fwd);
    const dist = surface === null ? this.spherical.radius : Math.min(surface, this.spherical.radius);

    const fov = (this.camera.fov * Math.PI) / 180;
    const worldPerPixel = (2 * Math.tan(fov / 2) * Math.max(dist, MIN_DISTANCE)) / Math.max(1, viewportHeight);

    _right.setFromMatrixColumn(this.camera.matrix, 0);
    _up.setFromMatrixColumn(this.camera.matrix, 1);

    const move = _move
      .set(0, 0, 0)
      .addScaledVector(_right, -dxPx * worldPerPixel)
      .addScaledVector(_up, dyPx * worldPerPixel);

    this.target.add(move);
    this.camera.position.add(move);
    this.camera.updateMatrixWorld();
    this.dirty = true;
  }

  /**
   * Wheel dolly toward the point under the cursor.
   *
   * Camera AND pivot are scaled uniformly about that point, which (a) keeps the
   * camera->pivot direction exactly as it was, so nothing rotates and the pivot
   * stays on the view axis, (b) shrinks the radius by the same factor, and
   * (c) drifts the pivot onto whatever you zoomed at, which is what makes
   * close-in orbiting land where the user expects. The step is a fixed fraction
   * of the distance to that surface, so it eases in asymptotically instead of
   * punching through the wall or stalling 50 m out because the pivot is far.
   */
  dolly(wheelDeltaY: number, ndc?: THREE.Vector2 | null): void {
    const dir = this.cursorDirection(ndc, _dir);
    const surface = this.probeAhead(dir);
    // Floor the anchor at a millimetre, NOT at MIN_DISTANCE: clamping it to
    // 0.25 m put the anchor behind a wall the camera was 0.2 m from, which
    // defeated the gap clamp below and let the wheel walk through in 45 mm
    // steps (measured: 0.247 -> 0.201 -> 0.156 -> 0.111 -> 0.065 m).
    const anchorDist = Math.max(surface ?? this.spherical.radius, 1e-3);

    let f = Math.exp(wheelDeltaY * WHEEL_RATE);
    if (f < 1) {
      // Zooming in: leave at least MIN_SURFACE_GAP between camera and surface.
      f = Math.max(f, Math.min(1, MIN_SURFACE_GAP / anchorDist));
    }
    const newRadius = THREE.MathUtils.clamp(this.spherical.radius * f, MIN_DISTANCE, MAX_DISTANCE);
    f = newRadius / this.spherical.radius;
    if (!Number.isFinite(f) || f === 1) return;

    _anchor.copy(this.camera.position).addScaledVector(dir, anchorDist);
    // target <- anchor + f * (target - anchor)
    this.target.sub(_anchor).multiplyScalar(f).add(_anchor);
    this.spherical.radius = newRadius;
    this.applySpherical();
  }

  // -- pivot ----------------------------------------------------------------

  /**
   * Put the pivot `d` metres down the view axis. The camera does not move and
   * does not rotate: the pivot was already on that axis, so only the radius
   * changes, and the angles are re-derived from the same orientation.
   */
  setPivotDistance(d: number): void {
    const r = THREE.MathUtils.clamp(d, MIN_DISTANCE, MAX_DISTANCE);
    this.camera.getWorldDirection(_fwd);
    this.target.copy(this.camera.position).addScaledVector(_fwd, r);
    this.spherical.setFromVector3(_offset.copy(this.camera.position).sub(this.target));
    this.spherical.radius = r;
    this.spherical.phi = Math.max(EPS, Math.min(Math.PI - EPS, this.spherical.phi));
    this.applySpherical();
  }

  /**
   * Re-anchor the pivot to the surface in the middle of the screen. Called when
   * an orbit drag starts, so orbiting pivots around what the user is looking at
   * rather than around a tile centre half a kilometre behind the wall in front
   * of them. Returns false (and changes nothing) when the view axis hits sky.
   */
  recenterPivot(): boolean {
    this.camera.getWorldDirection(_fwd);
    const d = this.probeAhead(_fwd);
    if (d === null || d < MIN_DISTANCE) return false;
    // Already within a few percent? Leave it; re-deriving the angles every drag
    // would churn the float state for nothing.
    if (Math.abs(d - this.spherical.radius) < this.spherical.radius * 0.02) return false;
    this.setPivotDistance(d);
    return true;
  }

  /**
   * Re-anchor the pivot to a world point, projected onto the view axis so the
   * camera neither moves nor rotates. Used after a click-pick: clicking a wall
   * a metre away makes the wheel and the next orbit work at that wall's scale.
   */
  setPivotAtPoint(p: THREE.Vector3): boolean {
    this.camera.getWorldDirection(_fwd);
    const d = _offset.copy(p).sub(this.camera.position).dot(_fwd);
    if (!(d > MIN_DISTANCE)) return false;
    this.setPivotDistance(d);
    return true;
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
    if (!this.flying) {
      this.smoothedSpeed = 0;
      return false;
    }

    this.camera.getWorldDirection(_fwd);
    _right.setFromMatrixColumn(this.camera.matrix, 0);

    const move = _move.set(0, 0, 0);
    if (this.keys.has("KeyW")) move.addScaledVector(_fwd, 1);
    if (this.keys.has("KeyS")) move.addScaledVector(_fwd, -1);
    if (this.keys.has("KeyD")) move.addScaledVector(_right, 1);
    if (this.keys.has("KeyA")) move.addScaledVector(_right, -1);
    if (this.keys.has("KeyE")) move.y += 1;
    if (this.keys.has("KeyQ")) move.y -= 1;
    if (move.lengthSq() === 0) return false;
    move.normalize();

    // Probe along the DIRECTION OF TRAVEL, not the view axis: that is what
    // keeps backing out of a tight spot quick while approaching a wall head-on
    // stays slow, and it is why a surface behind the camera can never speed it
    // up or slow it down.
    const target = this.baseSpeed(this.reference(_dir.copy(move)));
    if (this.smoothedSpeed <= 0) {
      this.smoothedSpeed = target; // first frame of a burst: no ramp-in
    } else if (target < this.smoothedSpeed) {
      this.smoothedSpeed += (target - this.smoothedSpeed) * Math.min(1, dt * SPEED_DOWN_RATE);
    } else {
      this.smoothedSpeed = Math.min(target, this.smoothedSpeed * Math.exp(dt * SPEED_UP_LN));
    }

    move.multiplyScalar(this.smoothedSpeed * this.keyModifier() * dt);
    this.camera.position.add(move);
    this.target.add(move);
    this.camera.updateMatrixWorld();
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
    this.invalidateSurface();
    this.applySpherical();
  }

  /** Frame a box from a fresh three-quarter viewpoint. */
  frameBoxDefaultAngle(box: THREE.Box3, fill = 1.4): void {
    this.spherical.theta = Math.PI * 0.25;
    this.spherical.phi = Math.PI * 0.33;
    this.frameBox(box, fill);
  }

  private applySpherical(): void {
    const offset = _offset.setFromSpherical(this.spherical);
    this.camera.position.copy(this.target).add(offset);
    this.camera.lookAt(this.target);
    this.camera.updateMatrixWorld();
    this.dirty = true;
  }
}
