/**
 * Materials for the instanced model batches.
 *
 * Two problems in the zone data drive everything in this file.
 *
 * 1. MIRRORED INSTANCES. Level art mirrors props by baking a negative scale
 *    into the instance basis, so the 4x4 has a negative-determinant 3x3.
 *    Measured on real sargasso zones: 157/2793 instances in `global_art`
 *    (5.6%), 79/1204 in `tile_n17_art` (6.6%), 42/2198 in `tile_o18_art`
 *    (1.9%). A negative determinant reverses the triangle winding *on screen*
 *    but three.js only checks the determinant of an object's `matrixWorld`
 *    (WebGLRenderer.js: `const frontFaceCW = object.isMesh &&
 *    object.matrixWorld.determinant() < 0`), never the per-instance matrix. So
 *    with a single-sided material the *near* surface of every mirrored
 *    instance is culled: a one-sided sheet disappears outright (measured: 0 of
 *    576 lit pixels for a mirrored plane, against 576 for the same plane
 *    un-mirrored), and a closed prop shows its far interior surface instead,
 *    with the wrong depth and the wrong shading over 57% of its footprint
 *    (measured across the 157 mirrored instances of `global_art`, mean
 *    luminance error 30/255).
 *
 *    The fix is `side: DoubleSide` on the model batches, which also does the
 *    right thing for the many single-sided sheets in this art (walls, cards,
 *    ground planes). What it costs: back-face culling is off, so closed
 *    geometry rasterises and shades roughly twice the fragments before the
 *    depth test rejects them. Vertex work, instance buffers and draw-call
 *    counts are unchanged — the cost is fragment-bound overdraw only. Measured
 *    with EXT_disjoint_timer_query_webgl2 on `global_art` (2793 instances, 5.5M
 *    triangles on screen at 799x866): 0.60 ms/frame single-sided vs 0.83
 *    ms/frame double-sided, i.e. +38% GPU time but only +0.23 ms against a
 *    16.7 ms vsync budget.
 *
 * 2. LIGHTING OF MIRRORED INSTANCES. Fixing visibility is not enough. With
 *    `DOUBLE_SIDED` three multiplies the shading normal by `faceDirection`
 *    (`gl_FrontFacing ? 1.0 : -1.0`, see ShaderChunk/normal_fragment_begin) so
 *    the lit normal always faces the camera. On a mirrored instance the
 *    visible surface is the one that reads as *back*-facing on screen, so
 *    `faceDirection` is -1 there and the outward normal gets inverted: the
 *    prop lights up inside-out. Multiplying by the sign of the instance
 *    determinant undoes exactly that, and stays correct in all four cases
 *    (mirrored/not x front/back).
 *
 * Note on non-uniform scale: three 0.180 already handles it in the instancing
 * path. ShaderChunk/defaultnormal_vertex divides the normal by
 * `vec3(dot(im[0],im[0]), dot(im[1],im[1]), dot(im[2],im[2]))` before
 * multiplying by `mat3(instanceMatrix)`, which is the inverse-transpose for a
 * rotation-and-scale basis. It is only wrong under shear, and the zone bases
 * are effectively shear-free (max |cos| between basis rows measured at 0.052,
 * with 7 of 2793 instances above 1e-3). So no per-instance normal matrix is
 * needed — only the mirror sign above.
 */

import * as THREE from "three";

/** How model geometry is shaded. */
export type ShadingMode = "textured" | "lit" | "normals" | "facing";

export const SHADING_MODES: ShadingMode[] = ["textured", "lit", "normals", "facing"];

/** True for the modes that sample a material's albedo map. */
export function usesTextures(mode: ShadingMode): boolean {
  return mode === "textured";
}

/**
 * Declare the mirror-sign varying and fill it from the instance matrix.
 *
 * `onBeforeCompile` runs before three resolves `#include` directives, so the
 * chunk names below are still literal text in the source at this point.
 * `instanceMatrix` is declared by the program prefix whenever `USE_INSTANCING`
 * is set, and `begin_vertex` is present in every stock vertex shader we use.
 */
function patchInstanceSign(shader: { vertexShader: string; fragmentShader: string }): void {
  shader.vertexShader = shader.vertexShader
    .replace("void main() {", "varying float vInstanceSign;\nvoid main() {")
    .replace(
      "#include <begin_vertex>",
      `#include <begin_vertex>
      #ifdef USE_INSTANCING
        vInstanceSign = determinant( mat3( instanceMatrix ) ) < 0.0 ? -1.0 : 1.0;
      #else
        vInstanceSign = 1.0;
      #endif`,
    );
  shader.fragmentShader = shader.fragmentShader.replace(
    "void main() {",
    "varying float vInstanceSign;\nvoid main() {",
  );
}

/** Cancel the `faceDirection` flip that DOUBLE_SIDED applies on mirrored instances. */
function patchMirroredNormal(material: THREE.Material, cacheKey: string): void {
  material.onBeforeCompile = (shader) => {
    patchInstanceSign(shader);
    shader.fragmentShader = shader.fragmentShader.replace(
      "#include <normal_fragment_begin>",
      `#include <normal_fragment_begin>
      normal *= vInstanceSign;`,
    );
  };
  // Keep the patched program out of the cache slot of an unpatched one.
  material.customProgramCacheKey = () => cacheKey;
}

// ---------------------------------------------------------------------------
// The `gbl_rgb_blend` family
// ---------------------------------------------------------------------------

/** One layer's inputs, in the order base, r, g, b. */
export interface BlendLayerInput {
  /** Null while the fetch is in flight, or for good if it will not decode. */
  texture: THREE.Texture | null;
  uvScale: readonly [number, number];
  /** `[hue, saturation, value]`, 0.5 neutral. Named HSL, computed as HSV. */
  hsl: readonly [number, number, number];
  /** `[mask gain, height softness]`. `[1, 0.33]` is the shipped default. */
  blend: readonly [number, number];
  /** Non-zero flips this layer's splat channel. */
  invert: number;
}

/** Everything the blend shader reads. See {@link createModelMaterial}. */
export interface BlendInput {
  /** The RGB splat / id mask. Null until it lands: the base layer shows alone. */
  mask: THREE.Texture | null;
  /** Multiplies the mask's UV. The mask has its own tiling constant. */
  maskUvScale: readonly [number, number];
  /** Exactly four, base first. */
  layers: BlendLayerInput[];
}

/** Where the per-material uniform objects live between rebuilds. */
interface BlendUniforms {
  rcraMask: { value: THREE.Texture | null };
  rcraLayerR: { value: THREE.Texture | null };
  rcraLayerG: { value: THREE.Texture | null };
  rcraLayerB: { value: THREE.Texture | null };
  rcraUv: { value: THREE.Vector2[] };
  rcraMaskUv: { value: THREE.Vector2 };
  rcraHsv: { value: THREE.Vector3[] };
  /** Per splat layer: `(mask gain, height softness, invert)`. */
  rcraBlend: { value: THREE.Vector3[] };
  /** Per-channel weight multiplier; 0 hides a layer that has not arrived. */
  rcraWeight: { value: THREE.Vector3 };
  /** 1 when the mask upload is sRGB-decoded by the sampler and must be undone. */
  rcraMaskSrgb: { value: number };
  /** Stand-in for the per-texel height. See {@link BLEND_HEIGHT}. */
  rcraHeight: { value: number };
}

/**
 * GLSL for the blend, spliced into MeshPhongMaterial's `map_fragment`.
 *
 * WHAT IT COMPUTES. This is the shipped pixel shader's arithmetic, not a fit.
 * `gbl_rgb_blend.materialgraph` carries its compiled programs in DAT1 section
 * `0xBBFC8900`, indexed by the 18-slot table `0x8C049CCA`; slot 4 is the
 * deferred G-buffer pixel shader and its SM5 half disassembles with
 * `fxc /dumpbin`. `src-tauri/src/bin/shader_dump.rs` extracts it and maps every
 * `cb2[r].c` back to the template constant at `buffer_offset == r*16 + c*4` and
 * every `tN` back to sampler slot `N - 5`, so the disassembly reads in terms of
 * the same hashes `core/model_materials.rs` resolves. Slots 5 and 14 agree, and
 * so does `gbl_rgb_blend_optimized`, which puts the same constants in different
 * registers.
 *
 *   uv_i = uv * uvScale_i
 *   out  = tint( sample( base, uv_0 ), hsv_0 )
 *   for i in r, g, b:
 *     m  = mix( mask[i], 1 - mask[i], ceil( invert_i ) )
 *     lo = saturate( H - softness_i )
 *     t  = saturate( ( gain_i * m - lo ) / ( H - lo ) )
 *     out = mix( out, tint( sample( layer_i, uv_i ), hsv_i ), t*t*(3-2t) )
 *
 * Three sequential lerps over a base, NOT a partition of the three channels —
 * which is also what the splat maps say (over the 21 distinct maps a Nefarious
 * City art tile binds, R+G+B averages 0.80, only 78% of texels keep it at or
 * below 1, single texels reach 2.9, and 17.5% have all three channels dark,
 * which is where the base is meant to show). What the height does is turn a
 * soft mask into a near-hard region id, which is why a mask can be soft and the
 * surface still read as distinct materials.
 *
 * THE TINT IS HSV, not HSL, despite the slot names: the shader's third axis is
 * `V = max(r,g,b)`. It runs in LINEAR space — the colour maps are `*_UNORM_SRGB`
 * so the sampler has already decoded them, and the shader re-encodes with
 * `pow(x, 1/2.2334)` only where it wants the *raw* stored bytes back (the
 * packed tangent normal). `saturation` and `value` are multiplied by `2*const`
 * and deliberately NOT clamped before the mix, so a saturation above 1
 * extrapolates past the hue ramp and only the final clamp bites; clamping early
 * gives visibly different colours. The shipped `rgb2hsv` carries no epsilon and
 * returns inf/NaN on pure grey and pure black, so both denominators get one
 * here.
 *
 * THE MASK IS NOT A COLOUR. `level_texture` serves PNG and the cache tags every
 * PNG sRGB, so the GPU sampler decodes the splat map's bytes through the sRGB
 * EOTF before this shader ever sees them — which would turn an authored 0.5
 * region weight into 0.21. `rcraMaskSrgb` re-applies the OETF, recovering the
 * authored 0..1 value to within 8-bit precision, and is driven from the
 * texture's own `colorSpace` so an RCTX payload that is already linear is left
 * alone. (Doing it here rather than by uploading the mask twice keeps one
 * upload per texture id, which is the whole point of the shared cache.)
 *
 * TWO APPROXIMATIONS, both forced by what the texture service can hand us:
 *
 *  1. ONE UV SET. The mask and the shared normal sample TEXCOORD0; all four
 *     colour layers sample TEXCOORD1. `rcms.ts` uploads UV0 only, so both are
 *     evaluated against UV0 here. The four colour layers stay correct relative
 *     to *each other*, which is what the tiling fix is about; only their
 *     relationship to the mask can drift.
 *  2. A CONSTANT HEIGHT. The real `H` is `sqrt(slice2.g)` of whichever colour
 *     layer is on top — every colour slot is a four-slice `Texture2DArray`
 *     (albedo / gloss / misc+height / tangent normal) and `level_texture`
 *     serves slice 0. `rcraHeight` stands in for it; see {@link BLEND_HEIGHT}.
 *     (The shared `_h` sampler is NOT the source — `0x3C651BCB` is declared and
 *     never sampled by any compiled program of this graph.)
 */
const BLEND_DECLS = /* glsl */ `
uniform sampler2D rcraMask;
uniform sampler2D rcraLayerR;
uniform sampler2D rcraLayerG;
uniform sampler2D rcraLayerB;
uniform vec2 rcraUv[ 4 ];
uniform vec2 rcraMaskUv;
uniform vec3 rcraHsv[ 4 ];
uniform vec3 rcraBlend[ 3 ];
uniform vec3 rcraWeight;
uniform float rcraMaskSrgb;
uniform float rcraHeight;

vec3 rcraLinearToSrgb( vec3 c ) {
  c = max( c, vec3( 0.0 ) );
  return mix( c * 12.92, 1.055 * pow( c, vec3( 0.41666667 ) ) - 0.055, step( vec3( 0.0031308 ), c ) );
}

// The branchless rgb2hsv the shader runs, plus the epsilon it omits.
vec3 rcraRgbToHsv( vec3 c ) {
  vec4 K = vec4( 0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0 );
  vec4 p = mix( vec4( c.bg, K.wz ), vec4( c.gb, K.xy ), step( c.b, c.g ) );
  vec4 q = mix( vec4( p.xyw, c.r ), vec4( c.r, p.yzx ), step( p.x, c.r ) );
  float d = q.x - min( q.w, q.y );
  return vec3( abs( q.z + ( q.w - q.y ) / ( 6.0 * d + 1e-10 ) ), d / ( q.x + 1e-10 ), q.x );
}

vec3 rcraTint( vec3 rgb, vec3 k ) {
  vec3 hsv = rcraRgbToHsv( rgb );
  float h = fract( hsv.x + k.x - 0.5 );
  // Unclamped on purpose: mix() below extrapolates for s > 1, exactly as the
  // shipped shader does, and only the final clamp bites.
  float s = 2.0 * k.y * hsv.y;
  float v = 2.0 * k.z * hsv.z;
  vec3 ramp = clamp( abs( mod( h * 6.0 + vec3( 0.0, 4.0, 2.0 ), 6.0 ) - 3.0 ) - 1.0, 0.0, 1.0 );
  return clamp( v * mix( vec3( 1.0 ), ramp, s ), 0.0, 1.0 );
}

vec3 rcraLayer( sampler2D tex, vec2 uv, int i ) {
  vec3 c = texture2D( tex, uv * rcraUv[ i ] ).rgb;
  // A neutral triple is the identity, so skip the round trip for it.
  if ( all( lessThan( abs( rcraHsv[ i ] - 0.5 ), vec3( 0.002 ) ) ) ) return c;
  return rcraTint( c, rcraHsv[ i ] );
}

// (gain, softness, invert) -> the smoothstepped weight this layer lerps with.
float rcraBlendWeight( float m, vec3 b ) {
  m = mix( m, 1.0 - m, ceil( b.z ) );
  float lo = clamp( rcraHeight - b.y, 0.0, 1.0 );
  // The shipped shader divides by (H - lo) unguarded, which is 0 wherever the
  // height is 0. Ours cannot be, but the clamp costs nothing and says so.
  float t = clamp( ( b.x * m - lo ) / max( rcraHeight - lo, 1e-5 ), 0.0, 1.0 );
  return t * t * ( 3.0 - 2.0 * t );
}
`;

const BLEND_MAP_FRAGMENT = /* glsl */ `
#ifdef USE_MAP
  vec3 rcraM = clamp( texture2D( rcraMask, vMapUv * rcraMaskUv ).rgb, 0.0, 1.0 );
  rcraM = mix( rcraM, rcraLinearToSrgb( rcraM ), rcraMaskSrgb );
  vec3 rcraOut = rcraLayer( map, vMapUv, 0 );
  // rcraWeight multiplies the WEIGHT, not the mask: an inverted layer shows
  // where its channel is dark, so zeroing the channel would not hide it.
  rcraOut = mix( rcraOut, rcraLayer( rcraLayerR, vMapUv, 1 ), rcraBlendWeight( rcraM.r, rcraBlend[ 0 ] ) * rcraWeight.r );
  rcraOut = mix( rcraOut, rcraLayer( rcraLayerG, vMapUv, 2 ), rcraBlendWeight( rcraM.g, rcraBlend[ 1 ] ) * rcraWeight.g );
  rcraOut = mix( rcraOut, rcraLayer( rcraLayerB, vMapUv, 3 ), rcraBlendWeight( rcraM.b, rcraBlend[ 2 ] ) * rcraWeight.b );
  diffuseColor.rgb *= rcraOut;
#endif
`;

/** Is this one of the materials {@link createModelMaterial} built for a blend? */
export function isBlendMaterial(m: THREE.Material): boolean {
  return Boolean((m.userData as { rcraBlend?: BlendUniforms }).rcraBlend);
}

/**
 * Whether the per-layer hue/saturation/value shift is applied. **On.**
 *
 * It used to ship off, on the grounds that the arithmetic was a guess. It is
 * not one any more — it is read off the compiled G-buffer pixel shader (see
 * the note above {@link BLEND_DECLS}) — and leaving it off was the worse error
 * of the two. Layers routinely share one texture and are told apart ONLY by
 * their shift: `ard_cargo_arch_painted_metal_trim_003` binds
 * `textures/sbs/paint/paint_002_c` to the base, red AND green layers, and the
 * three read as bright paint (value 1.0, i.e. x2), near-black grime (saturation
 * 0.059, value 0.200) and a punchier tinted paint (0.556 / 0.632 / 0.592).
 * Neutralised, all three collapse into one flat colour, which is the large
 * unbroken red the screenshot showed. `gbl_rgb_hsl_blend` is the extreme case:
 * one colour sampler and all four triples.
 *
 * The toggle stays for debugging — flipping it off is the fastest way to tell a
 * tint problem from a mask or tiling problem — and is reachable from a dev
 * console as `window.rcraBlendHsl(false)`.
 */
let hslEnabled = true;

/** Toggle the per-layer HSV shift. Returns the new state. */
export function setBlendHsl(on: boolean): boolean {
  hslEnabled = on;
  return hslEnabled;
}

export function blendHslEnabled(): boolean {
  return hslEnabled;
}

if (typeof window !== "undefined") {
  (window as unknown as Record<string, unknown>).rcraBlendHsl = setBlendHsl;
}

/**
 * The height the blend weights are evaluated at, standing in for a per-texel one.
 *
 * The shipped shader reads `H = sqrt(slice2.g)` of the colour layer currently on
 * top — the colour slots are four-slice `Texture2DArray`s and slice 2 is the
 * packed misc map. `level_texture` decodes slice 0, so there is no per-texel
 * height to read here and `H` is a constant.
 *
 * What it controls: a layer starts showing where `gain * mask > saturate(H -
 * softness)` and reaches full strength `min(H, softness)` later, so H slides the
 * ramp. 0.5 with the shipped defaults `(1.0, 0.33)` puts it at mask 0.17..0.50,
 * i.e. the layer wins wherever its channel is at or above a half — which is
 * where most splat texels already sit, 55% of them being within 0.1 of 0 or 1.
 * It is a midpoint, not a measurement, and it is the one number in this file
 * that is still a choice; it stops being one as soon as the texture service can
 * serve an array slice.
 */
export const BLEND_HEIGHT = 0.5;

/**
 * Push new textures / constants into an existing blend material.
 *
 * The uniform objects are shared by reference with the compiled program's
 * `shader.uniforms`, so this takes effect on the next frame WITHOUT a recompile
 * — which is what makes progressive layer loading free. A sampler uniform left
 * null binds three's empty texture, and its channel weight is 0, so an
 * unfinished layer is invisible rather than wrong.
 */
export function updateBlendMaterial(m: THREE.Material, blend: BlendInput): void {
  const u = (m.userData as { rcraBlend?: BlendUniforms }).rcraBlend;
  if (!u) return;
  const layers = blend.layers;
  const base = layers[0]?.texture ?? null;
  (m as THREE.MeshPhongMaterial).map = base;
  u.rcraMask.value = blend.mask;
  u.rcraLayerR.value = layers[1]?.texture ?? null;
  u.rcraLayerG.value = layers[2]?.texture ?? null;
  u.rcraLayerB.value = layers[3]?.texture ?? null;
  u.rcraMaskUv.value.set(blend.maskUvScale?.[0] ?? 1, blend.maskUvScale?.[1] ?? 1);
  for (let i = 0; i < 4; i++) {
    const l = layers[i];
    // The base layer's tiling is `0x8D2566DE`, a real constant like the other
    // three — it used to resolve to nothing, so the base drew at raw UV while
    // the other layers were tiled, and stretched across whole surfaces.
    u.rcraUv.value[i].set(l?.uvScale[0] ?? 1, l?.uvScale[1] ?? 1);
    // Neutral triples make `rcraLayer` take its early-out, so turning the shift
    // off costs nothing and needs no recompile.
    if (hslEnabled) {
      u.rcraHsv.value[i].set(l?.hsl[0] ?? 0.5, l?.hsl[1] ?? 0.5, l?.hsl[2] ?? 0.5);
    } else {
      u.rcraHsv.value[i].set(0.5, 0.5, 0.5);
    }
    if (i > 0) {
      u.rcraBlend.value[i - 1].set(l?.blend[0] ?? 1, l?.blend[1] ?? 0.33, l?.invert ?? 0);
    }
  }
  // A layer with no texture yet contributes nothing, so the base shows through
  // exactly as it did before this change — the "nothing regresses while
  // loading" guarantee, per channel rather than per material.
  u.rcraWeight.value.set(
    blend.mask && layers[1]?.texture ? 1 : 0,
    blend.mask && layers[2]?.texture ? 1 : 0,
    blend.mask && layers[3]?.texture ? 1 : 0,
  );
  u.rcraMaskSrgb.value = blend.mask && blend.mask.colorSpace === THREE.SRGBColorSpace ? 1 : 0;
  u.rcraHeight.value = BLEND_HEIGHT;
}

/**
 * Extras for one model-batch material. All optional; all only meaningful in
 * `textured` mode.
 */
export interface ModelMaterialOptions {
  /** Albedo map. Absent/null falls back to the flat hue below. */
  map?: THREE.Texture | null;
  /**
   * Draw this submesh as a `gbl_rgb_blend` surface instead of a single albedo.
   * Requires `layers[0].texture` to be resident — that is the map `map` would
   * otherwise have carried, and the one every fragment samples.
   */
  blend?: BlendInput | null;
  /**
   * The flat per-model hue, baked into `material.color`.
   *
   * In `lit` mode the hue rides on the per-instance colour, which is also what
   * carries selection and hover. That cannot work in `textured` mode: the
   * instance colour must be white there or it would tint every texture, so a
   * submesh with no texture would render white and vanish into the background.
   * Putting the hue in the material instead keeps the *untextured* parts of a
   * partly-textured model readable while the textured parts stay true colour,
   * and leaves selection/hover free to keep using the instance colour.
   */
  tint?: THREE.Color | null;
}

/**
 * The material for one model batch (or, in `textured` mode, one submesh group).
 *
 * `textured` the material's albedo map under the same lighting rig as `lit`.
 *            Lit, not unlit: an albedo shown flat reads as a painted cut-out
 *            and hides every geometric error the other modes exist to find.
 * `lit`      soft key/fill/rim clay shading, per-instance tint from the caller.
 * `normals`  view-space normals packed to RGB (three's MeshNormalMaterial) —
 *            the fastest way to spot a flipped, handedness-swapped or
 *            mis-decoded normal buffer.
 * `facing`   green where the camera sees the outward side of a triangle,
 *            magenta where it sees the inward side. Reads winding directly, so
 *            an inside-out mesh or a mirrored instance is obvious at a glance.
 */
export function createModelMaterial(
  mode: ShadingMode,
  wireframe: boolean,
  opts?: ModelMaterialOptions,
): THREE.Material {
  switch (mode) {
    case "textured": {
      const blend = opts?.blend ?? null;
      if (blend && blend.layers[0]?.texture) return createBlendMaterial(wireframe, blend);
      const map = opts?.map ?? null;
      const m = new THREE.MeshPhongMaterial({
        // White under a map so the texture shows its own colour; the flat hue
        // otherwise, so an untextured submesh still reads as "this model".
        color: map ? 0xffffff : (opts?.tint ?? new THREE.Color(0xffffff)),
        map,
        // Albedo maps already carry most of the material's character, so the
        // specular lobe drops well below the untextured `lit` one — at `lit`
        // strength it reads as a wet sheen over every surface.
        specular: 0x0b0e13,
        shininess: 6,
        side: THREE.DoubleSide,
        wireframe,
      });
      patchMirroredNormal(m, "rcra-textured");
      return m;
    }
    case "normals": {
      const m = new THREE.MeshNormalMaterial({ side: THREE.DoubleSide, wireframe });
      patchMirroredNormal(m, "rcra-normals");
      return m;
    }
    case "facing": {
      const m = new THREE.MeshBasicMaterial({ side: THREE.DoubleSide, wireframe });
      m.onBeforeCompile = (shader) => {
        patchInstanceSign(shader);
        // `color_fragment` has just folded in the per-instance tint; replace it
        // outright so the readout is purely geometric.
        shader.fragmentShader = shader.fragmentShader.replace(
          "#include <color_fragment>",
          `#include <color_fragment>
          float rcraFacing = ( gl_FrontFacing ? 1.0 : -1.0 ) * vInstanceSign;
          // Deliberately dark linear values: the viewport tone-maps and encodes
          // to sRGB, which lifts these to a clearly readable green / magenta.
          diffuseColor.rgb = rcraFacing > 0.0 ? vec3( 0.035, 0.230, 0.075 ) : vec3( 0.330, 0.015, 0.115 );`,
        );
      };
      m.customProgramCacheKey = () => "rcra-facing";
      return m;
    }
    case "lit":
    default: {
      // Phong over Lambert: a low, dim specular lobe costs almost nothing and
      // gives untextured clay enough of a highlight to read curvature and
      // panel breaks, which flat Lambert hides completely.
      const m = new THREE.MeshPhongMaterial({
        color: 0xffffff,
        specular: 0x141a22,
        shininess: 8,
        side: THREE.DoubleSide,
        wireframe,
      });
      patchMirroredNormal(m, "rcra-lit");
      return m;
    }
  }
}

/**
 * A `textured` material that evaluates the RGB-splat blend.
 *
 * It is a MeshPhongMaterial with `onBeforeCompile` doing the work, NOT a
 * hand-rolled ShaderMaterial: the lighting rig, fog, tone mapping, the sRGB
 * output encoding and the per-instance mirror-sign patch are all three's stock
 * chunks, and re-implementing them to change what feeds `diffuseColor` would
 * mean re-implementing every one of them. The splice lands in `map_fragment`,
 * which runs after `diffuse` and the per-instance `color_fragment` and before
 * every lighting chunk — so selection and hover still tint a blended surface,
 * and the blend result is lit exactly like a single albedo.
 *
 * One program serves every blended submesh in the level: all four layers, the
 * mask, the tiling and the HSL triples are uniforms, so the cache key is
 * constant and the shader compiles once.
 */
function createBlendMaterial(wireframe: boolean, blend: BlendInput): THREE.Material {
  const u: BlendUniforms = {
    rcraMask: { value: null },
    rcraLayerR: { value: null },
    rcraLayerG: { value: null },
    rcraLayerB: { value: null },
    rcraUv: { value: [0, 0, 0, 0].map(() => new THREE.Vector2(1, 1)) },
    rcraMaskUv: { value: new THREE.Vector2(1, 1) },
    rcraHsv: { value: [0, 0, 0, 0].map(() => new THREE.Vector3(0.5, 0.5, 0.5)) },
    rcraBlend: { value: [0, 0, 0].map(() => new THREE.Vector3(1, 0.33, 0)) },
    rcraWeight: { value: new THREE.Vector3(0, 0, 0) },
    rcraMaskSrgb: { value: 0 },
    rcraHeight: { value: BLEND_HEIGHT },
  };
  const m = new THREE.MeshPhongMaterial({
    color: 0xffffff,
    // Same lobe as the single-albedo path: a blended surface is still albedo.
    specular: 0x0b0e13,
    shininess: 6,
    side: THREE.DoubleSide,
    wireframe,
  });
  m.userData.rcraBlend = u;
  updateBlendMaterial(m, blend);
  m.onBeforeCompile = (shader) => {
    Object.assign(shader.uniforms, u);
    patchInstanceSign(shader);
    shader.fragmentShader = shader.fragmentShader
      .replace("void main() {", `${BLEND_DECLS}\nvoid main() {`)
      .replace("#include <map_fragment>", BLEND_MAP_FRAGMENT)
      .replace(
        "#include <normal_fragment_begin>",
        `#include <normal_fragment_begin>
      normal *= vInstanceSign;`,
      );
  };
  m.customProgramCacheKey = () => "rcra-textured-blend";
  return m;
}

// ---------------------------------------------------------------------------
// Block-2 geometry (impostor shells, projected decal sheets)
// ---------------------------------------------------------------------------

/** The two scene-object kinds that own a mesh inside the zone's raw block 2. */
export type BlockGeoKind = "impostor" | "decal";

/**
 * Per-instance tints. These are the *only* thing separating block-2 geometry
 * from model instances on screen — model instances take a per-model pastel from
 * `colorForKey`, whose hues are spread over the whole wheel at S=0.28/L=0.62,
 * so a saturated blue-steel and a saturated violet read as deliberate rather
 * than as "one more model". They match the kind swatches in the viewport
 * legend (`KIND_COLORS`), so a decal sheet and a decal marker are the same
 * colour whether the mesh has streamed in yet or not.
 */
export const BLOCK_GEO_TINTS: Record<BlockGeoKind, number> = {
  impostor: 0x6cb6ff,
  decal: 0xa371f7,
};

/**
 * Depth bias, as a fraction of the vertex's distance from the eye.
 *
 * Both kinds sit *exactly* on other geometry: an impostor is a baked
 * distant-view shell of the very buildings it overlaps, and a decal is a mesh
 * projected onto the surface it decorates. Coplanar surfaces z-fight.
 *
 * `polygonOffset` cannot fix it here. The viewport runs with
 * `logarithmicDepthBuffer: true`, so three's `logdepthbuf_fragment` chunk
 * writes `gl_FragDepth` in the fragment shader; a shader-written depth replaces
 * the rasteriser's value outright, and polygon offset is applied to the
 * rasteriser's value. So the offset silently does nothing.
 *
 * Biasing the *view-space* position instead works with any depth encoding:
 * `mvPosition` is measured from the eye, so scaling its xyz by `1 - bias` moves
 * the vertex along its own view ray — a positive bias pulls it toward the eye,
 * a negative one pushes it away — while its screen position is unchanged.
 * Being proportional to distance it stays effective at 5 m and at 5 km, which
 * matters when impostors are kilometre-scale skyline.
 *
 * Signs are chosen so the *more detailed* surface wins each overlap: decals are
 * pulled nearer (the sheet draws on top of the surface it decorates), impostors
 * are pushed away (the real model draws in front of its own stand-in).
 *
 * Magnitude, measured: on savali `tile_c17_art`, a centre-screen ray from 20 m
 * above the decal field hits the terrain model at 19.18 m and the decal at
 * 19.20 m — a 2 cm separation, i.e. 0.10% of the view distance, which is
 * exactly the coplanar case. 0.002 clears it by 2x (4 cm at 20 m) and is far
 * below any parallax the eye can see. Without it the decals were invisible:
 * over that view, correcting the sign moved 17.3% of the frame's pixels.
 *
 * The impostor sign is a guard rather than an observed fix — the shipped
 * impostors live in their own dedicated `*impostors*` zones (all 205 block-2
 * objects of megalopolis `tile_m37_art` and all 218 of nefarious
 * `tile_e19_art` are decals), so an impostor and the model it stands in for
 * are not currently loaded at the same time.
 */
const DEPTH_BIAS: Record<BlockGeoKind, number> = {
  impostor: -0.003,
  decal: 0.002,
};

/**
 * The bias one batch draws with, so CPU-side picking can rank hits the same way
 * the depth buffer does. Model batches draw unbiased, hence 0.
 */
export function depthBiasFor(kind: BlockGeoKind | null): number {
  return kind ? DEPTH_BIAS[kind] : 0;
}

function patchDepthBias(shader: { vertexShader: string }, bias: number): void {
  // `project_vertex` leaves `vec4 mvPosition` in scope and has already applied
  // `instanceMatrix` and `modelViewMatrix`; `logdepthbuf_vertex` runs after it
  // and re-reads `gl_Position`, so recomputing here is picked up downstream.
  shader.vertexShader = shader.vertexShader.replace(
    "#include <project_vertex>",
    `#include <project_vertex>
      mvPosition.xyz *= ${(1 - bias).toFixed(6)};
      gl_Position = projectionMatrix * mvPosition;`,
  );
}

/**
 * Material for one block-2 geometry batch.
 *
 * Shares the shading modes with the model batches so `normals` and `facing`
 * still diagnose an impostor or decal, and shares the double-sided +
 * mirror-sign treatment (both kinds are open sheets or shells, and both can sit
 * on a mirrored basis). What it adds is the depth bias above and, in `lit`
 * mode, a flatter response: impostors are lighting-baked billboardy shells, so
 * a strong specular on them reads as a bug.
 */
/**
 * What a block-2 mesh is tinted in textured mode: a neutral grey that sits with
 * real albedo instead of competing with it. The saturated kind tints stay for
 * the other modes, where they are the legend.
 *
 * MEASURED, and the reason this is dark rather than the light grey it was.
 * Block-2 meshes carry no material, so in `textured` mode they are the only
 * thing on screen wearing a flat constant — and a decal sheet is a big flat
 * quad lying on a wall or a floor. At 0xb9bec6 with a lifted emissive they came
 * out brighter than every real albedo around them: sampling one Nefarious City
 * interior view of `tile_d17_art` on an 8px grid, 11% of the lit frame was
 * near-white (max channel > 190, saturation < 0.18) and **81% of that was
 * block-2 geometry** — the pale near-white slabs in the bug report. They are not
 * `default_pink` placeholders and not a failed load; they are these.
 *
 * 0x6b7078 is roughly a third of the old luminance, which puts a decal sheet
 * below the mid-tone of the architecture it lies on instead of above it, while
 * still reading as deliberate untextured geometry rather than as a shadow.
 */
export const BLOCK_GEO_NEUTRAL = 0x6b7078;

export function createBlockGeoMaterial(
  kind: BlockGeoKind,
  mode: ShadingMode,
  wireframe: boolean,
): THREE.Material {
  const bias = DEPTH_BIAS[kind];
  const cacheKey = `rcra-blockgeo-${kind}-${mode}`;

  switch (mode) {
    case "normals": {
      const m = new THREE.MeshNormalMaterial({ side: THREE.DoubleSide, wireframe });
      m.onBeforeCompile = (shader) => {
        patchInstanceSign(shader);
        patchDepthBias(shader, bias);
        shader.fragmentShader = shader.fragmentShader.replace(
          "#include <normal_fragment_begin>",
          `#include <normal_fragment_begin>
          normal *= vInstanceSign;`,
        );
      };
      m.customProgramCacheKey = () => cacheKey;
      return m;
    }
    case "facing": {
      const m = new THREE.MeshBasicMaterial({ side: THREE.DoubleSide, wireframe });
      m.onBeforeCompile = (shader) => {
        patchInstanceSign(shader);
        patchDepthBias(shader, bias);
        shader.fragmentShader = shader.fragmentShader.replace(
          "#include <color_fragment>",
          `#include <color_fragment>
          float rcraFacing = ( gl_FrontFacing ? 1.0 : -1.0 ) * vInstanceSign;
          diffuseColor.rgb = rcraFacing > 0.0 ? vec3( 0.035, 0.230, 0.075 ) : vec3( 0.330, 0.015, 0.115 );`,
        );
      };
      m.customProgramCacheKey = () => cacheKey;
      return m;
    }
    case "lit":
    case "textured":
    default: {
      // Block-2 meshes carry no material of their own, so there is nothing to
      // texture them with. In `lit` they wear their kind tint, which is how the
      // legend reads them. In `textured` that tint would be the only saturated
      // thing on screen — 438 impostor shells glowing blue over a city of real
      // albedo — so they drop to neutral grey and simply read as untextured
      // geometry, which is what they are.
      const textured = mode === "textured";
      const m = new THREE.MeshPhongMaterial({
        // White base: the per-instance colour carries the tint, so selection
        // and hover can recolour a block-geo object exactly like a model.
        color: 0xffffff,
        specular: kind === "impostor" ? 0x090c10 : 0x101418,
        shininess: kind === "impostor" ? 2 : 6,
        // Keeps unlit faces of a baked shell from going to black, and gives
        // decals the slight self-lit look they have in game.
        // No emissive lift in textured mode: it was what pushed these sheets
        // past the surrounding albedo. The other modes keep it, because there
        // the kind tint IS the legend and wants to be readable.
        emissive: textured ? 0x000000 : BLOCK_GEO_TINTS[kind],
        emissiveIntensity: textured ? 0 : kind === "impostor" ? 0.14 : 0.2,
        side: THREE.DoubleSide,
        wireframe,
      });
      m.onBeforeCompile = (shader) => {
        patchInstanceSign(shader);
        patchDepthBias(shader, bias);
        shader.fragmentShader = shader.fragmentShader.replace(
          "#include <normal_fragment_begin>",
          `#include <normal_fragment_begin>
          normal *= vInstanceSign;`,
        );
      };
      m.customProgramCacheKey = () => cacheKey;
      return m;
    }
  }
}

/**
 * Key / fill / rim rig, returned so the caller can add it to the scene.
 *
 * A hemisphere alone (what this used to be, at intensity 2.4) washes the scene
 * into uniform pale clay and hides exactly the shading errors we are hunting.
 * Here the hemisphere only fills the shadows, and three directionals from
 * clearly separated directions and colours carry the form: a warm key from
 * above-front-right, a cool fill from below-left-back, and a dim rim from
 * behind to separate silhouettes against the dark background.
 */
export function createLightRig(): THREE.Light[] {
  const hemi = new THREE.HemisphereLight(0xa8c4e8, 0x2a2620, 0.9);

  const key = new THREE.DirectionalLight(0xfff2e0, 2.0);
  key.position.set(0.55, 1.0, 0.35);

  const fill = new THREE.DirectionalLight(0x93b4de, 0.7);
  fill.position.set(-0.7, 0.15, -0.55);

  const rim = new THREE.DirectionalLight(0xdce8ff, 0.55);
  rim.position.set(-0.25, -0.55, 0.8);

  return [hemi, key, fill, rim];
}
