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
  /** `[hue, saturation, luminance]`, 0.5 neutral. */
  hsl: readonly [number, number, number];
}

/** Everything the blend shader reads. See {@link createModelMaterial}. */
export interface BlendInput {
  /** The RGB splat / id mask. Null until it lands: the base layer shows alone. */
  mask: THREE.Texture | null;
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
  rcraHsl: { value: THREE.Vector3[] };
  /** Per-channel weight multiplier; 0 hides a layer that has not arrived. */
  rcraWeight: { value: THREE.Vector3 };
  /** 1 when the mask upload is sRGB-decoded by the sampler and must be undone. */
  rcraMaskSrgb: { value: number };
}

/**
 * GLSL for the blend, spliced into MeshPhongMaterial's `map_fragment`.
 *
 * WHAT IT COMPUTES, and why it is shaped this way:
 *
 *   uv_i = uv0 * uvScale_i ; c_i = hsl( sample( layer_i, uv_i ), hsl_i )
 *   out  = c_base ; out = mix( out, c_r, m.r ) ; …g ; …b
 *
 * Three independent weights lerped over a base, NOT a partition of the three
 * channels. Measured over the 21 distinct splat maps a Nefarious City art tile
 * binds: R+G+B averages 0.80 but only 78% of texels keep it at or below 1 and
 * single texels reach 2.9, so the channels cannot be normalised weights; and
 * 17.5% of texels have all three channels dark, which is precisely where the
 * base layer is meant to show through. The lerp ORDER (r, then g, then b) only
 * matters where two channels overlap and is a convention, not a measurement.
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
 * THE HSL SHIFT is the part the data does not pin down: all twelve constants
 * ship at 0.5 and shipped materials keep them inside 0..1, so they are
 * 0.5-neutral sliders, but the arithmetic the graph performs on them is not
 * recoverable from the archives. `hue += h - 0.5`, `saturation *= 2s`,
 * `luminance *= 2l` reproduces both the neutral point and the range, and is the
 * usual convention. It is applied in LINEAR space, because the sampler has
 * would have operated on. A neutral triple short-circuits, so the common case
 * costs one uniform compare rather than two colour-space conversions.
 */
const BLEND_DECLS = /* glsl */ `
uniform sampler2D rcraMask;
uniform sampler2D rcraLayerR;
uniform sampler2D rcraLayerG;
uniform sampler2D rcraLayerB;
uniform vec2 rcraUv[ 4 ];
uniform vec3 rcraHsl[ 4 ];
uniform vec3 rcraWeight;
uniform float rcraMaskSrgb;

vec3 rcraLinearToSrgb( vec3 c ) {
  c = max( c, vec3( 0.0 ) );
  return mix( c * 12.92, 1.055 * pow( c, vec3( 0.41666667 ) ) - 0.055, step( vec3( 0.0031308 ), c ) );
}

vec3 rcraRgbToHsl( vec3 c ) {
  float mx = max( c.r, max( c.g, c.b ) );
  float mn = min( c.r, min( c.g, c.b ) );
  float l = ( mx + mn ) * 0.5;
  float d = mx - mn;
  float s = d <= 0.0 ? 0.0 : d / max( 1.0 - abs( 2.0 * l - 1.0 ), 1e-5 );
  float h = 0.0;
  if ( d > 0.0 ) {
    if ( mx == c.r ) h = mod( ( c.g - c.b ) / d, 6.0 );
    else if ( mx == c.g ) h = ( c.b - c.r ) / d + 2.0;
    else h = ( c.r - c.g ) / d + 4.0;
    h /= 6.0;
  }
  return vec3( h, s, l );
}

vec3 rcraHslToRgb( vec3 hsl ) {
  float h = fract( hsl.x ) * 6.0;
  float c = ( 1.0 - abs( 2.0 * hsl.z - 1.0 ) ) * hsl.y;
  float x = c * ( 1.0 - abs( mod( h, 2.0 ) - 1.0 ) );
  vec3 rgb;
  if ( h < 1.0 ) rgb = vec3( c, x, 0.0 );
  else if ( h < 2.0 ) rgb = vec3( x, c, 0.0 );
  else if ( h < 3.0 ) rgb = vec3( 0.0, c, x );
  else if ( h < 4.0 ) rgb = vec3( 0.0, x, c );
  else if ( h < 5.0 ) rgb = vec3( x, 0.0, c );
  else rgb = vec3( c, 0.0, x );
  return rgb + ( hsl.z - 0.5 * c );
}

vec3 rcraShift( vec3 rgb, vec3 hsl ) {
  if ( all( lessThan( abs( hsl - 0.5 ), vec3( 0.002 ) ) ) ) return rgb;
  vec3 v = rcraRgbToHsl( clamp( rgb, 0.0, 1.0 ) );
  v.x = fract( v.x + hsl.x - 0.5 );
  v.y = clamp( v.y * hsl.y * 2.0, 0.0, 1.0 );
  v.z = clamp( v.z * hsl.z * 2.0, 0.0, 1.0 );
  return rcraHslToRgb( v );
}

vec3 rcraLayer( sampler2D tex, vec2 uv, int i ) {
  return rcraShift( texture2D( tex, uv * rcraUv[ i ] ).rgb, rcraHsl[ i ] );
}
`;

const BLEND_MAP_FRAGMENT = /* glsl */ `
#ifdef USE_MAP
  vec3 rcraM = clamp( texture2D( rcraMask, vMapUv ).rgb, 0.0, 1.0 );
  rcraM = mix( rcraM, rcraLinearToSrgb( rcraM ), rcraMaskSrgb ) * rcraWeight;
  vec3 rcraOut = rcraLayer( map, vMapUv, 0 );
  rcraOut = mix( rcraOut, rcraLayer( rcraLayerR, vMapUv, 1 ), rcraM.r );
  rcraOut = mix( rcraOut, rcraLayer( rcraLayerG, vMapUv, 2 ), rcraM.g );
  rcraOut = mix( rcraOut, rcraLayer( rcraLayerB, vMapUv, 3 ), rcraM.b );
  diffuseColor.rgb *= rcraOut;
#endif
`;

/** Is this one of the materials {@link createModelMaterial} built for a blend? */
export function isBlendMaterial(m: THREE.Material): boolean {
  return Boolean((m.userData as { rcraBlend?: BlendUniforms }).rcraBlend);
}

/**
 * Push new textures / constants into an existing blend material.
 *
 * The uniform objects are shared by reference with the compiled program's
 * `shader.uniforms`, so this takes effect on the next frame WITHOUT a recompile
 * — which is what makes progressive layer loading free. A sampler uniform left
 * null binds three's empty texture, and its channel weight is 0, so an
 * unfinished layer is invisible rather than wrong.
 */
/**
 * Whether the per-layer hue/saturation/luminance shift is applied.
 *
 * Off by default, deliberately. The blend *structure* — the splat mask, the four
 * layers, their tilings — is recovered from the shipped template and verified.
 * The HSL ARITHMETIC is not: `hue += h - 0.5`, `sat *= 2s`, `lum *= 2l` matches
 * the neutral point and the 0..1 range and nothing else, because the expression
 * lives in a compiled shader nobody has disassembled. It is also the dominant
 * term in the picture — 23% of layers carry a hue constant more than 60 degrees
 * off neutral, and at close range it rotates Nefarious City's red architectural
 * trim to teal, which the mask blend alone does not do.
 *
 * So the evidenced half ships on and the guessed half ships off, until the
 * arithmetic is settled by disassembling the pixel shader.
 */
let hslEnabled = false;

/** Toggle the per-layer HSL shift. Returns the new state. */
export function setBlendHsl(on: boolean): boolean {
  hslEnabled = on;
  return hslEnabled;
}

export function blendHslEnabled(): boolean {
  return hslEnabled;
}

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
  for (let i = 0; i < 4; i++) {
    const l = layers[i];
    u.rcraUv.value[i].set(l?.uvScale[0] ?? 1, l?.uvScale[1] ?? 1);
    // Neutral triples make `rcraShift` a no-op on its own early-out, so turning
    // the shift off costs nothing and needs no recompile.
    if (hslEnabled) {
      u.rcraHsl.value[i].set(l?.hsl[0] ?? 0.5, l?.hsl[1] ?? 0.5, l?.hsl[2] ?? 0.5);
    } else {
      u.rcraHsl.value[i].set(0.5, 0.5, 0.5);
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
    rcraHsl: { value: [0, 0, 0, 0].map(() => new THREE.Vector3(0.5, 0.5, 0.5)) },
    rcraWeight: { value: new THREE.Vector3(0, 0, 0) },
    rcraMaskSrgb: { value: 0 },
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
 */
export const BLOCK_GEO_NEUTRAL = 0xb9bec6;

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
        emissive: textured ? 0x2a2e34 : BLOCK_GEO_TINTS[kind],
        emissiveIntensity: textured ? 0.10 : kind === "impostor" ? 0.14 : 0.2,
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
