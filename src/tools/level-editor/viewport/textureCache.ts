/**
 * Shared, budgeted texture cache for the level viewport.
 *
 * WHY A SHARED CACHE IS NOT OPTIONAL. A planet reuses a small texture set over
 * an enormous number of surfaces: the models of one savali art tile resolve to
 * a few hundred distinct `.texture` assets across tens of thousands of
 * submeshes, and every zone of the region draws from the same set. Keying on
 * the texture asset id at module level means a texture is fetched, decoded and
 * uploaded exactly once for the life of the process, no matter how many models,
 * batches or zones point at it. Refcounts (not liveness guesses) decide when
 * the GPU memory goes back.
 *
 * VRAM BUDGET. Two separate limits, because they fail differently.
 *
 *   {@link TEXTURE_MAX_SIZE} = 1024 is the resolution we ASK for. RCRA ships
 *   albedo up to 4096 on hero assets and the streamed HD tiles of one 4096 BC7
 *   texture are 16 MB on their own, while at editor viewing distances — an
 *   orbit camera framing a whole tile, where a 2 m prop covers a few hundred
 *   pixels — nothing past 1024 is ever sampled. The cap is also what keeps the
 *   backend inside the RESIDENT mip chain instead of pulling streamed tiles:
 *   measured over savali's whole open area (1,536 models, 256 distinct albedo
 *   assets), asking for 1024 returns 193 textures at 512, 28 at 256, and only
 *   4 at 1024 — 383 MiB of RGBA8-plus-mips for an entire planet region.
 *
 *   {@link TEXTURE_BUDGET_BYTES} = 1.25 GB is the hard stop on total uploads,
 *   ~8% of a 16 GB card. Against the measurement above that holds roughly three
 *   savali-sized regions at once; one busy art tile (savali `tile_c17_art`, 79
 *   models) costs 40 textures and 43.5 MB, and nine adjacent tiles (300 models,
 *   57,884 instances) still cost only 52 textures and 56.6 MB, because the
 *   textures are shared. Past the budget the cache stops requesting and the
 *   affected submeshes keep the flat hue rather than the frame rate collapsing
 *   into a swap storm — a visible, honest degradation the HUD reports.
 *
 * DECODING. `level_texture` serves a PNG today, which is the branch the shipped
 * path exercises. The payload is sniffed rather than assumed, so the RCTX
 * container documented in `api.ts` and a bare DDS decode here too — that is the
 * route to handing over the still-compressed BCn blocks, which would cut the
 * numbers above by a further 4x and remove the decode on both sides. Compressed
 * payloads upload as compressed textures when the context exposes the matching
 * extension, and are skipped (flat hue) rather than silently mis-decoded when
 * it does not.
 */

import * as THREE from "three";
import type { TextureLoader } from "../types";

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/** Largest edge we ask the backend for. See the file header for why 1024. */
export const TEXTURE_MAX_SIZE = 1024;

/** Hard cap on total uploaded texture bytes. See the file header. */
export const TEXTURE_BUDGET_BYTES = 1.25 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// RCTX container
// ---------------------------------------------------------------------------

export const RCTX_MAGIC = 0x58544352; // "RCTX"
export const RCTX_HEADER_BYTES = 32;

/** `RctxHeader.format` values. */
export const RCTX_RGBA8 = 0;
export const RCTX_PNG = 1;
export const RCTX_BC1 = 2;
export const RCTX_BC3 = 3;
export const RCTX_BC4 = 4;
export const RCTX_BC5 = 5;
export const RCTX_BC7 = 6;
export const RCTX_BC6H = 7;

/** `RctxHeader.flags` bits. */
export const RCTX_FLAG_SRGB = 1 << 0;
export const RCTX_FLAG_HAS_ALPHA = 1 << 1;

interface DecodedTexture {
  texture: THREE.Texture;
  /** Approximate bytes this upload occupies on the GPU. */
  bytes: number;
}

export interface TextureCacheStats {
  /** Distinct textures currently resident. */
  count: number;
  /** Approximate GPU bytes those uploads occupy. */
  bytes: number;
  /** Requests still in flight or queued. */
  loading: number;
  /** Textures refused because the budget was already spent. */
  skipped: number;
  /** Textures that came back empty or would not decode. */
  failed: number;
}

// ---------------------------------------------------------------------------
// Module-level store
// ---------------------------------------------------------------------------

interface Entry {
  texture: THREE.Texture | null; // null = known-undecodable / refused
  bytes: number;
  refs: number;
}

const cache = new Map<string, Entry>();
let residentBytes = 0;
let skippedCount = 0;
let failedCount = 0;

/**
 * GPU capability probe, filled by {@link setTextureContext}.
 *
 * The renderer owns the WebGL context, so it hands us the pieces the decoder
 * needs: which compressed formats can be uploaded at all, and the anisotropy
 * ceiling (level art is nearly all ground planes and walls seen at a glancing
 * angle — without anisotropy those go to mush two metres past the camera).
 */
let compressedFormats = new Set<number>();
let maxAnisotropy = 1;

export function setTextureContext(renderer: THREE.WebGLRenderer): void {
  const gl = renderer.getContext();
  const formats = new Set<number>();
  const has = (name: string) => Boolean(gl.getExtension(name));
  if (has("WEBGL_compressed_texture_s3tc")) {
    formats.add(RCTX_BC1);
    formats.add(RCTX_BC3);
  }
  if (has("EXT_texture_compression_rgtc")) {
    formats.add(RCTX_BC4);
    formats.add(RCTX_BC5);
  }
  if (has("EXT_texture_compression_bptc")) {
    formats.add(RCTX_BC7);
    formats.add(RCTX_BC6H);
  }
  compressedFormats = formats;
  maxAnisotropy = renderer.capabilities.getMaxAnisotropy();
}

export function textureCacheStats(inFlight = 0): TextureCacheStats {
  return { count: cache.size, bytes: residentBytes, loading: inFlight, skipped: skippedCount, failed: failedCount };
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

function bytesPerLevel(format: number, w: number, h: number): number {
  switch (format) {
    case RCTX_BC1:
    case RCTX_BC4:
      return Math.max(1, Math.ceil(w / 4)) * Math.max(1, Math.ceil(h / 4)) * 8;
    case RCTX_BC3:
    case RCTX_BC5:
    case RCTX_BC7:
    case RCTX_BC6H:
      return Math.max(1, Math.ceil(w / 4)) * Math.max(1, Math.ceil(h / 4)) * 16;
    default:
      return w * h * 4;
  }
}

/** Full-chain size of a mipped upload: the 1 + 1/4 + 1/16 … series. */
function withMipTail(baseBytes: number, mipped: boolean): number {
  return mipped ? Math.round(baseBytes * (4 / 3)) : baseBytes;
}

const THREE_COMPRESSED: Record<number, number> = {
  [RCTX_BC1]: THREE.RGBA_S3TC_DXT1_Format,
  [RCTX_BC3]: THREE.RGBA_S3TC_DXT5_Format,
  [RCTX_BC4]: THREE.RED_RGTC1_Format,
  [RCTX_BC5]: THREE.RED_GREEN_RGTC2_Format,
  [RCTX_BC7]: THREE.RGBA_BPTC_Format,
  [RCTX_BC6H]: THREE.RGB_BPTC_UNSIGNED_Format,
};

/**
 * Orientation. The engine's UVs are DirectX-style: V grows downward from a
 * top-left origin (`VertexesSection::parse_scaled` reads a raw i16 pair and
 * only scales it, and `gltf_writer` passes `[u, v]` straight through into a
 * glTF, which defines exactly that convention). three.js instead defaults to
 * `flipY = true`, i.e. it flips the image at upload so that a bottom-left-origin
 * UV lands right. Feeding top-left UVs into a flipped image mirrors every
 * texture vertically. So every texture here is uploaded with `flipY = false`
 * and the decoded UVs are used untouched — the same pairing `GLTFLoader` uses.
 */
function finishTexture(t: THREE.Texture, srgb: boolean): THREE.Texture {
  t.wrapS = THREE.RepeatWrapping;
  t.wrapT = THREE.RepeatWrapping;
  t.flipY = false;
  t.colorSpace = srgb ? THREE.SRGBColorSpace : THREE.NoColorSpace;
  t.anisotropy = Math.min(8, maxAnisotropy);
  t.needsUpdate = true;
  return t;
}

function isPng(b: Uint8Array): boolean {
  return b.length > 8 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47;
}

function isJpeg(b: Uint8Array): boolean {
  return b.length > 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff;
}

function isDds(b: Uint8Array): boolean {
  return b.length > 128 && b[0] === 0x44 && b[1] === 0x44 && b[2] === 0x53 && b[3] === 0x20;
}

async function decodeImageBlob(bytes: Uint8Array, mime: string): Promise<DecodedTexture | null> {
  const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
  const bitmap = await createImageBitmap(new Blob([buf], { type: mime }), {
    // Keep the bytes exactly as authored; three's flipY/colorSpace handling
    // does the rest, and a browser colour conversion here would double-apply.
    imageOrientation: "none",
    premultiplyAlpha: "none",
    colorSpaceConversion: "none",
  });
  const t = new THREE.Texture(bitmap as unknown as HTMLImageElement);
  t.generateMipmaps = true;
  t.minFilter = THREE.LinearMipmapLinearFilter;
  t.magFilter = THREE.LinearFilter;
  return { texture: finishTexture(t, true), bytes: withMipTail(bitmap.width * bitmap.height * 4, true) };
}

/**
 * DXGI format -> RCTX format, for a DDS payload. Only the codes the game's own
 * textures use are mapped; anything else returns -1 and the texture is skipped.
 */
function dxgiToRctx(dxgi: number): number {
  switch (dxgi) {
    case 70:
    case 71:
    case 72:
      return RCTX_BC1;
    case 76:
    case 77:
    case 78:
      return RCTX_BC3;
    case 79:
    case 80:
      return RCTX_BC4;
    case 82:
    case 83:
      return RCTX_BC5;
    case 94:
    case 95:
      return RCTX_BC6H;
    case 97:
    case 98:
    case 99:
      return RCTX_BC7;
    default:
      return -1;
  }
}

function decodeDds(bytes: Uint8Array): DecodedTexture | null {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const height = dv.getUint32(12, true);
  const width = dv.getUint32(16, true);
  const mipCount = Math.max(1, dv.getUint32(28, true));
  const fourCC = dv.getUint32(84, true);
  let format = -1;
  let dataOffset = 128;
  if (fourCC === 0x30315844) {
    // "DX10" — the extended header carries the real DXGI code.
    format = dxgiToRctx(dv.getUint32(128, true));
    dataOffset = 148;
  } else if (fourCC === 0x31545844) format = RCTX_BC1; // DXT1
  else if (fourCC === 0x35545844) format = RCTX_BC3; // DXT5
  if (format < 0) return null;
  return decodeCompressed(bytes.subarray(dataOffset), width, height, mipCount, format, true);
}

function decodeCompressed(
  data: Uint8Array,
  width: number,
  height: number,
  mipCount: number,
  format: number,
  srgb: boolean,
): DecodedTexture | null {
  const glFormat = THREE_COMPRESSED[format];
  if (glFormat === undefined || !compressedFormats.has(format)) return null;

  const mipmaps: { data: Uint8Array; width: number; height: number }[] = [];
  let o = 0;
  let w = width;
  let h = height;
  for (let i = 0; i < mipCount; i++) {
    const size = bytesPerLevel(format, w, h);
    if (o + size > data.length) break;
    mipmaps.push({ data: data.subarray(o, o + size), width: w, height: h });
    o += size;
    w = Math.max(1, w >> 1);
    h = Math.max(1, h >> 1);
  }
  if (!mipmaps.length) return null;

  const t = new THREE.CompressedTexture(
    mipmaps as unknown as ImageData[],
    width,
    height,
    glFormat as THREE.CompressedPixelFormat,
  );
  t.minFilter = mipmaps.length > 1 ? THREE.LinearMipmapLinearFilter : THREE.LinearFilter;
  t.magFilter = THREE.LinearFilter;
  // A compressed texture cannot have mips generated for it at upload time.
  t.generateMipmaps = false;
  return { texture: finishTexture(t, srgb), bytes: o };
}

function decodeRaw(
  data: Uint8Array,
  width: number,
  height: number,
  mipCount: number,
  srgb: boolean,
): DecodedTexture | null {
  const base = width * height * 4;
  if (data.length < base) return null;
  const t = new THREE.DataTexture(data.subarray(0, base), width, height, THREE.RGBAFormat, THREE.UnsignedByteType);
  t.minFilter = THREE.LinearMipmapLinearFilter;
  t.magFilter = THREE.LinearFilter;

  if (mipCount > 1) {
    // Supplied chain: three uploads each level as given.
    const levels: { data: Uint8Array; width: number; height: number }[] = [
      { data: data.subarray(0, base), width, height },
    ];
    let o = base;
    let w = Math.max(1, width >> 1);
    let h = Math.max(1, height >> 1);
    for (let i = 1; i < mipCount; i++) {
      const size = w * h * 4;
      if (o + size > data.length) break;
      levels.push({ data: data.subarray(o, o + size), width: w, height: h });
      o += size;
      w = Math.max(1, w >> 1);
      h = Math.max(1, h >> 1);
    }
    t.mipmaps = levels as unknown as ImageData[];
    t.generateMipmaps = false;
  } else {
    t.generateMipmaps = true;
  }
  return { texture: finishTexture(t, srgb), bytes: withMipTail(base, true) };
}

/** Decode whatever `level_texture` returned. `null` = no usable texture. */
export async function decodeTexturePayload(buf: ArrayBuffer): Promise<DecodedTexture | null> {
  if (!buf || buf.byteLength === 0) return null;
  const bytes = new Uint8Array(buf);

  if (isPng(bytes)) return decodeImageBlob(bytes, "image/png");
  if (isJpeg(bytes)) return decodeImageBlob(bytes, "image/jpeg");
  if (isDds(bytes)) return decodeDds(bytes);

  if (buf.byteLength < RCTX_HEADER_BYTES) return null;
  const dv = new DataView(buf);
  if (dv.getUint32(0, true) !== RCTX_MAGIC) return null;
  if (dv.getUint32(4, true) !== 1) return null;

  const width = dv.getUint32(8, true);
  const height = dv.getUint32(12, true);
  const format = dv.getUint32(16, true);
  const mipCount = Math.max(1, dv.getUint32(20, true));
  const flags = dv.getUint32(24, true);
  const dataBytes = dv.getUint32(28, true);
  if (width === 0 || height === 0 || dataBytes === 0) return null;
  if (RCTX_HEADER_BYTES + dataBytes > buf.byteLength) return null;

  const srgb = (flags & RCTX_FLAG_SRGB) !== 0;
  const data = bytes.subarray(RCTX_HEADER_BYTES, RCTX_HEADER_BYTES + dataBytes);

  if (format === RCTX_PNG) return decodeImageBlob(data, "image/png");
  if (format === RCTX_RGBA8) return decodeRaw(data, width, height, mipCount, srgb);
  return decodeCompressed(data, width, height, mipCount, format, srgb);
}

// ---------------------------------------------------------------------------
// The queue
// ---------------------------------------------------------------------------

export type TextureReadyCallback = (id: string, texture: THREE.Texture | null) => void;

/**
 * One queue per loaded scene, over the shared module-level cache.
 *
 * Mirrors {@link MeshQueue}: bounded concurrency, refcounted holds, and a
 * `dispose` that releases every hold so a zone swap frees the GPU memory of
 * textures nothing else references. Requests are deduplicated both within a
 * queue (`held` / `queued`) and across queues (the shared cache), so the same
 * texture is never fetched twice even when two zones stream at once.
 */
export class TextureQueue {
  private loader: TextureLoader;
  private onReady: TextureReadyCallback;
  private maxInFlight: number;

  private pending: string[] = [];
  private queued = new Set<string>();
  private inFlight = 0;
  private held = new Set<string>();
  private cancelled = false;

  get outstanding(): number {
    return this.pending.length + this.inFlight;
  }

  constructor(loader: TextureLoader, onReady: TextureReadyCallback, maxInFlight = 6) {
    this.loader = loader;
    this.onReady = onReady;
    this.maxInFlight = maxInFlight;
  }

  request(id: string): void {
    if (this.cancelled || !id || this.queued.has(id) || this.held.has(id)) return;

    const hit = cache.get(id);
    if (hit) {
      hit.refs++;
      this.held.add(id);
      const t = hit.texture;
      queueMicrotask(() => {
        if (!this.cancelled) this.onReady(id, t);
      });
      return;
    }

    this.queued.add(id);
    this.pending.push(id);
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

  private async load(id: string): Promise<void> {
    // Budget check happens at request time, not at decode time: refusing before
    // the fetch is what actually saves the memory.
    if (residentBytes >= TEXTURE_BUDGET_BYTES) {
      skippedCount++;
      cache.set(id, { texture: null, bytes: 0, refs: 1 });
      this.held.add(id);
      if (!this.cancelled) this.onReady(id, null);
      return;
    }

    let decoded: DecodedTexture | null = null;
    try {
      const buf = await this.loader(id, TEXTURE_MAX_SIZE);
      decoded = await decodeTexturePayload(buf);
    } catch (e) {
      console.warn(`[texture] load failed for ${id}:`, e);
      decoded = null;
    }
    if (!decoded) failedCount++;

    if (this.cancelled) {
      decoded?.texture.dispose();
      return;
    }
    // Another queue may have raced us to the same id.
    const existing = cache.get(id);
    if (existing) {
      decoded?.texture.dispose();
      existing.refs++;
      this.held.add(id);
      this.onReady(id, existing.texture);
      return;
    }

    cache.set(id, { texture: decoded?.texture ?? null, bytes: decoded?.bytes ?? 0, refs: 1 });
    residentBytes += decoded?.bytes ?? 0;
    this.held.add(id);
    this.onReady(id, decoded?.texture ?? null);
  }

  /** Release every hold; textures nobody else references are disposed. */
  dispose(): void {
    this.cancelled = true;
    this.pending.length = 0;
    this.queued.clear();
    for (const id of this.held) {
      const e = cache.get(id);
      if (!e) continue;
      e.refs--;
      if (e.refs <= 0) {
        e.texture?.dispose();
        residentBytes -= e.bytes;
        cache.delete(id);
      }
    }
    this.held.clear();
    if (residentBytes < 0) residentBytes = 0;
  }
}
