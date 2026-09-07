/**
 * Progressive mesh loader.
 *
 * A zone can reference hundreds of distinct models; fetching them all at once
 * would stall the UI thread on decode. This runs a small number of loads in
 * flight, in the order the caller enqueues them (the backend hands us models
 * sorted by instance count, so the visually dominant ones arrive first), and
 * calls back per model as geometry becomes available.
 *
 * Decoded geometries are shared across zones through a module-level cache keyed
 * by model id, with refcounts so switching zones does not re-decode meshes but
 * also does not leak GPU buffers for models nobody references any more.
 */

import * as THREE from "three";
import { decodeRcms, rcmsToGeometry } from "./rcms";
import type { MeshLoader } from "../types";

interface CacheEntry {
  geometry: THREE.BufferGeometry | null; // null = known-undecodable
  refs: number;
}

const cache = new Map<string, CacheEntry>();

function acquire(modelId: string): CacheEntry | undefined {
  const e = cache.get(modelId);
  if (e) e.refs++;
  return e;
}

function release(modelId: string): void {
  const e = cache.get(modelId);
  if (!e) return;
  e.refs--;
  if (e.refs <= 0) {
    e.geometry?.dispose();
    cache.delete(modelId);
  }
}

export type MeshReadyCallback = (modelId: string, geometry: THREE.BufferGeometry | null) => void;

export class MeshQueue {
  private loader: MeshLoader;
  private onReady: MeshReadyCallback;
  private maxInFlight: number;

  private pending: string[] = [];
  private queued = new Set<string>();
  private inFlight = 0;
  private held = new Set<string>();
  private cancelled = false;

  /** Models still waiting or loading. */
  get outstanding(): number {
    return this.pending.length + this.inFlight;
  }

  constructor(loader: MeshLoader, onReady: MeshReadyCallback, maxInFlight = 4) {
    this.loader = loader;
    this.onReady = onReady;
    this.maxInFlight = maxInFlight;
  }

  /**
   * Request a model. If it is already decoded the callback fires synchronously
   * on the next microtask; otherwise it is queued.
   */
  request(modelId: string): void {
    if (this.cancelled || this.queued.has(modelId) || this.held.has(modelId)) return;

    const hit = acquire(modelId);
    if (hit) {
      this.held.add(modelId);
      const g = hit.geometry;
      queueMicrotask(() => {
        if (!this.cancelled) this.onReady(modelId, g);
      });
      return;
    }

    this.queued.add(modelId);
    this.pending.push(modelId);
    this.pump();
  }

  private pump(): void {
    while (!this.cancelled && this.inFlight < this.maxInFlight && this.pending.length > 0) {
      const id = this.pending.shift()!;
      this.inFlight++;
      this.load(id).finally(() => {
        this.inFlight--;
        this.queued.delete(id);
        this.pump();
      });
    }
  }

  private async load(modelId: string): Promise<void> {
    let geometry: THREE.BufferGeometry | null = null;
    try {
      const buf = await this.loader(modelId);
      const mesh = decodeRcms(buf);
      if (mesh) geometry = rcmsToGeometry(mesh);
    } catch {
      geometry = null;
    }
    if (this.cancelled) {
      geometry?.dispose();
      return;
    }
    // Another queue may have raced us to the same model.
    const existing = cache.get(modelId);
    if (existing) {
      geometry?.dispose();
      existing.refs++;
      this.held.add(modelId);
      this.onReady(modelId, existing.geometry);
      return;
    }
    cache.set(modelId, { geometry, refs: 1 });
    this.held.add(modelId);
    this.onReady(modelId, geometry);
  }

  /** Drop every reference this queue holds and stop loading. */
  dispose(): void {
    this.cancelled = true;
    this.pending.length = 0;
    this.queued.clear();
    for (const id of this.held) release(id);
    this.held.clear();
  }
}

/** Test hook — number of distinct models currently held in the shared cache. */
export function meshCacheSize(): number {
  return cache.size;
}
