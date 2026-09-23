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

/**
 * Extras for one model-batch material. All optional; all only meaningful in
 * `textured` mode.
 */
export interface ModelMaterialOptions {
  /** Albedo map. Absent/null falls back to the flat hue below. */
  map?: THREE.Texture | null;
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
