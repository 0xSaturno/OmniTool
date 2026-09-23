//! Resolving a model's material slots down to the textures they bind.
//!
//! The chain the level viewport needs, end to end:
//!
//! ```text
//! .model  Model Material (0x3250BB80)   slot i -> material path + Maya mapping name + asset id
//!   -> .material  Material Serialized Data (0xF5260180)   sampler hash -> bound .texture path
//!        Material Template Samplers     (0x1CAFE804)      sampler hash -> DEFAULT texture path
//!   -> .texture   Texture Header        (0x4EDE3593)      width / height / DXGI format
//! ```
//!
//! `slot i` is exactly what the RCMS submesh `materialIndex` indexes, so the
//! viewport can go from a draw call to a texture without another lookup table.
//!
//! **Where a slot name comes from.** The human-readable HLSL sampler name is
//! *not* shipped: the compiled `RDEF` blocks only carry `textureUnit8` /
//! `textureSampler8`, and the serialized sampler records are keyed by a CRC32
//! in a hash space of their own. What *is* shipped is the template's **default**
//! texture path per sampler, and its suffix names the semantic
//! (`…_c` albedo, `…_n` normal, `…_g` gloss, `…_m`/`…_mp` mask …). A
//! `.material` embeds a copy of its template's sampler table, so that mapping
//! is readable without opening the `.materialgraph` at all — and every sampler
//! hash seen anywhere is remembered in a process-wide table, so a material that
//! ships without the template section still gets real names as soon as any
//! other material has declared the same hash. Only when both of those fail does
//! this fall back to guessing from the *bound* texture's own suffix, which
//! [`SlotName::Guessed`] marks so the caller can tell the two apart.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dashmap::DashMap;
use log::debug;
use serde::Serialize;

use crate::core::crc64;
use crate::core::error::{Result, ToolkitError};
use crate::core::level::{self, LevelIndex};
use crate::core::material::{MaterialFile, MaterialSerialized, TAG_MATERIAL_SERIALIZED};
use crate::core::material_graph::MaterialTemplate;
use crate::core::material_names;
use crate::core::texture_serve::{self, TexInfo};
use crate::tools::model_converter::sections::looks::{MaterialSection, TAG_MATERIAL};

/// How many resolved models to keep. A planet has ~890 distinct models and each
/// record is a few hundred bytes, so the whole planet fits comfortably.
const MODEL_CACHE_CAP: usize = 4096;

// ---------------------------------------------------------------------------
// DTOs — the `level_model_materials` contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureRef {
    /// The sampler's semantic: `albedo` | `normal` | `roughness` | `mask` |
    /// `height` | `ao` | `id` | `emissive` | `detail` | `other`.
    pub slot: String,
    /// `0x`-prefixed 16-hex asset id, `"0x0"` when the path is empty.
    pub texture_id: String,
    /// Lowercase forward-slash asset path, `""` when unknown.
    pub texture_path: String,
    /// Native size — the first streamed mip when one exists, else the resident
    /// base. `0` when the asset is not in the archives.
    pub width: u32,
    pub height: u32,
    /// `DXGI_FORMAT` name, `""` when the asset is not in the archives.
    pub format: String,
    /// True when `slot` came from the material's own template table rather than
    /// from guessing at the bound texture's filename.
    pub named: bool,
    /// True when this binds one of the shipped placeholder textures
    /// (`default_pink_shader`, `mo_white_h`, `mo_black`, `default_grid`).
    pub placeholder: bool,
    /// True when the decoded texels are sRGB-encoded.
    pub srgb: bool,
    /// True when the texture asset was found in the TOC and its header parsed.
    pub resolved: bool,
}

// ---------------------------------------------------------------------------
// The `gbl_rgb_blend` family
// ---------------------------------------------------------------------------

/// One layer of an RGB-splat blend.
///
/// A layer always exists; a *texture of its own* does not. The lighter variants
/// of the template declare fewer samplers and let several layers read the same
/// map, differing only by their HSL shift — `gbl_rgb_hsl_blend` is the extreme
/// case, one texture recoloured four ways. `own_texture` says which happened.
/// That is also why the shift is not optional: with it neutral, those layers
/// collapse into one flat colour and the surface loses every substance break.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlendLayer {
    /// `base` | `r` | `g` | `b` — which splat channel fades this layer in.
    pub channel: String,
    /// `0x`-prefixed 16-hex asset id of the colour map this layer samples.
    pub texture_id: String,
    pub texture_path: String,
    /// False when the template declares no sampler for this layer, or the
    /// material leaves it unbound, and the base layer's map is used instead.
    pub own_texture: bool,
    /// Multiplies the **colour** UV set for this layer. `[1, 1]` when the
    /// material never overrode the template's default. All four layers —
    /// the base included — have one; see [`rgb_blend::LAYERS`].
    pub uv_scale: [f32; 2],
    /// `[hue, saturation, value]`, each `0..1` with **0.5 neutral** — the
    /// shipped default of all twelve of these constants. Named `_HSL_` in the
    /// graph, but the shader's third axis is HSV's *value*; see
    /// [`BlendRecipe`] for the exact transform.
    pub hsl: [f32; 3],
    /// `"measured"` when the triple was tied to this layer by recovering its
    /// declared name (`"<Channel>_HSL_<Hue|Saturation|Luminance>"`) from the
    /// CRC32 the template stores, `"assumed"` when the assignment is this
    /// module's convention and nothing contradicts it. All four are
    /// `"measured"` today; the field stays so a later family can be honest.
    pub hsl_source: String,
    /// `[mask gain, height softness]` — the two numbers that shape this layer's
    /// blend weight (see [`BlendRecipe`]). Ships as `[1.0, 0.33]`. The base
    /// layer has no such constant and reports the identity `[1.0, 0.33]`.
    pub blend: [f32; 2],
    /// `ceil()`ed to a flag by the shader: non-zero flips this layer's splat
    /// channel, so the layer shows where the mask is *dark*. 0 for the base.
    pub invert: f32,
    /// Intensity of this layer's own tangent normal, taken from slice 3 of its
    /// colour array. Ships as 1.0. The viewport does not apply it — it has no
    /// per-layer normal to scale, because it only ever fetches slice 0.
    pub normal_scale: f32,
}

/// What a `gbl_rgb_blend` material computes, in a form a renderer can evaluate.
///
/// # The blend
///
/// This is **read off the shipped pixel shader**, not fitted. A built
/// `.materialgraph` carries its compiled programs in section `0xBBFC8900`,
/// indexed by the 18-slot table `0x8C049CCA`; slot 4 is the deferred G-buffer
/// pixel shader (four render targets) and disassembles with `fxc /dumpbin`.
/// `src/bin/shader_dump.rs` extracts and annotates it, and
/// [`crate::core::material_graph`] carries the slot table this module's hashes
/// come from. Slots 5 and 14 compute the same blend.
///
/// ```text
/// mask = sample(splat_mask, uv0 * mask_uv_scale).rgb          // raw; the mask is not sRGB
/// h    = sqrt(sample(base_colour, uv1 * base.uv_scale, slice 2).g)
/// out  = tint(sample(base_colour, uv1 * base.uv_scale, slice 0), base.hsl)
/// for L in [r, g, b]:                                          // this order
///     m    = mix(mask[L], 1 - mask[L], ceil(L.invert))
///     lo   = saturate(h - L.blend[1])
///     t    = saturate((L.blend[0] * m - lo) / (h - lo))
///     out  = mix(out, tint(sample(L.colour, uv1 * L.uv_scale, slice 0), L.hsl),
///                t * t * (3 - 2 * t))
/// ```
///
/// So it *is* three sequential lerps over a base rather than a partition of the
/// three channels — which is what the splat maps themselves say (over 21
/// distinct maps of one Nefarious City art tile, `R+G+B` averages 0.80, only
/// 78% of texels keep it at or below 1, single texels reach 2.9, and 17.5% have
/// all three channels dark, which is where the base shows). What is new is that
/// each lerp's weight is not the raw channel: a **height** from the colour
/// array's own slice 2 contrast-boosts it into a near-hard region id, which is
/// why the shipped masks can be soft and the surface still reads as distinct
/// materials. `h - lo` is zero where the height is zero, so a renderer must
/// clamp that denominator.
///
/// **The shared height sampler is a red herring.** `0x3C651BCB` is declared and
/// never read by any compiled program of this graph; the height above comes from
/// whichever colour layer is on top. (The shader then stochastically swaps in
/// the winning layer's height for the next comparison; a viewer that keeps the
/// base's is close and deterministic.)
///
/// # The tint
///
/// The twelve constants are named `_HSL_` but the shader's third axis is HSV's
/// **value** — `V = max(r, g, b)`, not lightness. The transform, in **linear**
/// space (the colour slots are `*_UNORM_SRGB`, so the sampler has already
/// decoded them), with every constant defaulting to 0.5 = identity:
///
/// ```text
/// hsv = rgb_to_hsv(colour)
/// hue = frac(hsv.x + HUE - 0.5)          // additive and wrapped
/// sat = 2 * SATURATION * hsv.y           // plain multiply, NOT clamped here
/// val = 2 * LUMINANCE  * hsv.z           // plain multiply, NOT clamped here
/// out = saturate(val * mix(1, hue_ramp(hue), sat))
/// ```
///
/// `sat` above 1 deliberately extrapolates past the hue ramp and only the final
/// `saturate` clamps, so clamping it early gives visibly different colours. The
/// shipped `rgb_to_hsv` carries no epsilon and therefore produces `inf`/`NaN` on
/// pure grey and pure black; a viewer has to add one.
///
/// # Two UV sets
///
/// The splat mask and the shared normal sample `TEXCOORD0`; all four colour
/// layers sample `TEXCOORD1`. `uv_scale` above is therefore *not* a scale on the
/// same interpolant as `mask_uv_scale`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlendRecipe {
    /// Always `"rgb_blend"` today; the field exists so a second family can be
    /// added without changing the shape.
    pub kind: String,
    /// The `.materialgraph` this recipe was read from.
    pub template: String,
    /// The RGB splat / id mask. Shipped materials bind a `…_id` texture here
    /// in every case measured.
    pub mask_texture_id: String,
    pub mask_texture_path: String,
    /// Multiplies **UV0** for the mask and the shared normal — a different
    /// interpolant from the one the colour layers' `uv_scale` multiplies.
    pub mask_uv_scale: [f32; 2],
    /// base, r, g, b — in that order, always four entries.
    pub layers: Vec<BlendLayer>,
    /// The one normal map the whole surface shares.
    pub normal_texture_id: String,
    pub normal_texture_path: String,
    /// The one height / AO / misc map the whole surface shares — shipped
    /// materials bind `…_ao` or `…_h` here.
    pub misc_texture_id: String,
    pub misc_texture_path: String,
    /// Every constant the template declares, by `0x`-prefixed CRC32 of its
    /// (unrecoverable) declared name, with the value this material actually
    /// uses. Includes the ones already broken out above, so nothing the blend
    /// might need is lost to this module's reading of it.
    ///
    /// The groups that used to sit here unnamed are now broken out above: the
    /// per-layer `float2` is `(mask gain, height softness)` (`blend`), and the
    /// per-layer `float` that ships at 0 is `*_Splat_Mask_Invert` (`invert`).
    /// What is still unmodelled is the gloss chain — a `float2` gloss remap
    /// `(range, position)` per layer and a `float` switch choosing whether the
    /// colour array's slice 1 carries a specular colour plus alpha gloss or a
    /// single `sqrt`-decoded gloss — which a diffuse-only viewport has nothing
    /// to do with.
    pub params: BTreeMap<String, Vec<f32>>,
}

/// The `gbl_rgb_blend` interface, by the CRC32 the template declares each slot
/// under. These hashes are stable across the whole family — the texture *unit*
/// is not, so nothing here indexes by unit.
mod rgb_blend {
    /// RGB splat / id mask, sampled on **UV0** (`TEXCOORD0`). Shipped materials
    /// bind a `…_id` texture here in every case measured (37/37 on one Nefarious
    /// City art tile). Its R/G/B drive the red/green/blue layers, which the
    /// constants `E2F04E17` / `9E7FE3F1` / `7A50C706` — exactly
    /// `"{Red,Green,Blue}_Splat_Mask_Invert"` — name, and which the compiled
    /// shader confirms by reading `mask.x`/`.y`/`.z` next to each of them.
    pub const SPLAT: u32 = 0x205D_ECC3;
    pub const NORMAL: u32 = 0x61D0_637F;
    /// Height / AO / misc; its template default is `mo_white_h`. **Declared and
    /// never sampled** by any compiled program of this graph — the height that
    /// drives the blend comes from each colour layer's own array slice 2.
    pub const MISC: u32 = 0x3C65_1BCB;
    /// `float2` tiling for the splat mask *and* the shared normal, both of which
    /// sample UV0. Not a colour layer's tiling: `mul r8.xy, v1.xyxx, <28BB963E>`
    /// feeds only the `205DECC3` and `61D0637F` samplers.
    pub const MASK_UV: u32 = 0x28BB_963E;

    /// `(sampler, uv_scale float2, normal_scale float, [hue, saturation,
    ///   luminance], blend float2, splat_mask_invert float)` for base, red,
    /// green, blue.
    ///
    /// **Every hash here is now confirmed against the compiled G-buffer pixel
    /// shader** (`gbl_rgb_blend.materialgraph` program slot 4, disassembled with
    /// `fxc /dumpbin` and annotated by `shader_dump annotate`, which maps each
    /// `cb2[r].c` back to the template constant at `buffer_offset == r*16 + c*4`
    /// and each `tN` back to sampler slot `N - 5`). Two entries in this table
    /// used to be wrong, both fixed here:
    ///
    ///   * **The base layer's tiling is `8D2566DE`.** The disassembly packs two
    ///     tilings into one multiply — `mul r3.xyzw, v2.xyxy,
    ///     <1D93CDB7.x|1D93CDB7.y|8D2566DE.z|8D2566DE.w>` — and the `.zw` half
    ///     is what the base sampler `9E34A894` reads. Previously this was `0`
    ///     ("it is `8D2566DE` or `28BB963E` and nothing separates them"), so the
    ///     base was drawn at raw UV while the other three were tiled: a
    ///     stretched, smeared base across ~38% of the family's materials, which
    ///     are the ones that override it (observed range 0.5..2).
    ///   * **Green and blue were swapped.** The variant algebra that assigned
    ///     them — `_red_green` drops `E4CF0EAD`, therefore `E4CF0EAD` is blue —
    ///     is wrong, because a variant's *name* is not the mask channel it
    ///     keeps. The shader blends `E4CF0EAD` from `mask.y` with the
    ///     `Green_HSL_*` triple and `9E7FE3F1` (`Green_Splat_Mask_Invert`), and
    ///     `013DD88D` from `mask.z` with `Blue_HSL_*` and `7A50C706`. The HSL
    ///     triples were already right (they were pinned by inverting the CRC32
    ///     of their declared names, not by variant algebra) — so the old table
    ///     had each of those two layers reading the *other* layer's texture,
    ///     tiling and normal intensity under its own name.
    ///
    /// The tint constants stand as they were: the builder hashes
    /// `"<node variable>_<port>"`, and these twelve are exactly
    /// `"{Base,Red,Green,Blue}_HSL_{Hue,Saturation,Luminance}"`. Twelve
    /// simultaneous hits forming one naming scheme is not coincidence, and the
    /// shader independently drives each triple from the matching mask channel.
    pub const LAYERS: [(u32, u32, u32, [u32; 3], u32, u32); 4] = [
        // base — "Base_HSL_{Hue,Saturation,Luminance}"; no mask channel, so no
        // blend float2 and no invert.
        (0x9E34_A894, 0x8D25_66DE, 0x2356_CD0C, [0x886E_717D, 0x6A81_9148, 0x92F0_4E42], 0, 0),
        // red — mask.r
        (0x54C8_3B26, 0x1D93_CDB7, 0x1BD0_45A3, [0x18D8_DA14, 0x25B3_6543, 0x7FF6_17E1], 0xFAE5_46A6, 0xE2F0_4E17),
        // green — mask.g
        (0xE4CF_0EAD, 0x62D7_112C, 0xBBAB_D142, [0x679C_068F, 0x33AF_D592, 0x0F89_833E], 0x3124_B6DD, 0x9E7F_E3F1),
        // blue — mask.b
        (0x013D_D88D, 0x4BAF_5B9E, 0xD1C7_B633, [0x4EE4_4C3D, 0x3E1E_E9AB, 0xD3DF_84B4], 0xDB96_0F93, 0x7A50_C706),
    ];

    /// The identity of the `(mask gain, height softness)` pair, which is also its
    /// shipped default, and what the base layer reports.
    pub const BLEND_DEFAULT: [f32; 2] = [1.0, 0.33];

    pub const CHANNELS: [&str; 4] = ["base", "r", "g", "b"];
}

/// Read a `gbl_rgb_blend` recipe out of a template plus one material's
/// overrides, or `None` when the template is not of that family.
///
/// Recognition is by declared sampler hash, not by asset path, so a renamed or
/// project-local copy of the graph still resolves.
pub fn derive_blend(
    template: &MaterialTemplate,
    serialized: &MaterialSerialized,
    template_path: &str,
) -> Option<BlendRecipe> {
    if template.sampler_by_hash(rgb_blend::SPLAT).is_none() {
        return None;
    }
    if !rgb_blend::LAYERS
        .iter()
        .any(|(s, ..)| template.sampler_by_hash(*s).is_some())
    {
        return None;
    }

    // A sampler's effective path: what the material bound, else nothing. The
    // template's own default is a pink placeholder and never worth drawing.
    let bound = |hash: u32| -> Option<String> {
        let p = serialized
            .samplers
            .iter()
            .find(|s| s.name_hash == hash)
            .map(|s| crc64::normalize_path(&s.path))?;
        if p.is_empty() || is_placeholder_texture(&p) {
            None
        } else {
            Some(p)
        }
    };
    // A constant's effective value: the material's override, else the
    // template's default, else nothing.
    let value = |hash: u32| -> Option<Vec<f32>> {
        serialized
            .constants
            .iter()
            .find(|c| c.name_hash == hash)
            .map(|c| c.values.clone())
            .or_else(|| template.constant_by_hash(hash).map(|c| c.default_values.clone()))
    };

    let mask = bound(rgb_blend::SPLAT).unwrap_or_default();
    let normal = bound(rgb_blend::NORMAL).unwrap_or_default();
    let misc = bound(rgb_blend::MISC).unwrap_or_default();
    let base_tex = bound(rgb_blend::LAYERS[0].0);

    // `float2`s are stored as a pair; a template that ships a scalar broadcasts.
    let float2 = |hash: u32, fallback: [f32; 2]| -> [f32; 2] {
        match value(hash) {
            Some(v) if v.len() >= 2 => [v[0], v[1]],
            Some(v) if v.len() == 1 => [v[0], v[0]],
            _ => fallback,
        }
    };

    let mask_uv_scale = float2(rgb_blend::MASK_UV, [1.0, 1.0]);

    let mut layers = Vec::with_capacity(4);
    for (i, (sampler, uv, normal_scale, hsl, blend, invert)) in rgb_blend::LAYERS.iter().enumerate()
    {
        // The template must declare the layer at all — a three-layer variant
        // has no fourth layer, not a fourth layer sharing the base's map.
        if i > 0 && template.sampler_by_hash(*sampler).is_none() && template
            .constant_by_hash(hsl[0])
            .is_none()
        {
            continue;
        }
        let own = bound(*sampler);
        let path = own.clone().or_else(|| base_tex.clone()).unwrap_or_default();
        let hsl_values = [
            value(hsl[0]).and_then(|v| v.first().copied()).unwrap_or(0.5),
            value(hsl[1]).and_then(|v| v.first().copied()).unwrap_or(0.5),
            value(hsl[2]).and_then(|v| v.first().copied()).unwrap_or(0.5),
        ];
        layers.push(BlendLayer {
            channel: rgb_blend::CHANNELS[i].to_string(),
            texture_id: hex_id(texture_id_of(&path)),
            texture_path: path,
            own_texture: own.is_some(),
            uv_scale: float2(*uv, [1.0, 1.0]),
            hsl: hsl_values,
            // Every triple is tied to its layer by the recovered name, and the
            // compiled shader drives each one from the matching mask channel.
            hsl_source: "measured".into(),
            blend: float2(*blend, rgb_blend::BLEND_DEFAULT),
            invert: value(*invert).and_then(|v| v.first().copied()).unwrap_or(0.0),
            normal_scale: value(*normal_scale)
                .and_then(|v| v.first().copied())
                .unwrap_or(1.0),
        });
    }

    // Every declared constant, overridden or not, so the front end can probe
    // the ones this module has no name for.
    let mut params = BTreeMap::new();
    for c in &template.constants {
        if let Some(v) = value(c.name_hash) {
            params.insert(format!("{:#010X}", c.name_hash), v);
        }
    }
    // Constants a material sets that its embedded template copy does not list.
    for c in &serialized.constants {
        params
            .entry(format!("{:#010X}", c.name_hash))
            .or_insert_with(|| c.values.clone());
    }

    Some(BlendRecipe {
        kind: "rgb_blend".into(),
        template: crc64::normalize_path(template_path),
        mask_texture_id: hex_id(texture_id_of(&mask)),
        mask_texture_path: mask,
        mask_uv_scale,
        layers,
        normal_texture_id: hex_id(texture_id_of(&normal)),
        normal_texture_path: normal,
        misc_texture_id: hex_id(texture_id_of(&misc)),
        misc_texture_path: misc,
        params,
    })
}

fn texture_id_of(path: &str) -> u64 {
    if path.is_empty() {
        0
    } else {
        crc64::hash_raw(path.as_bytes())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialSlot {
    /// Matches the RCMS submesh `materialIndex`.
    pub index: u32,
    /// `0x`-prefixed 16-hex asset id of the `.material`; `"0x0"` when the model
    /// names no material for this slot at all.
    pub material_id: String,
    /// Lowercase forward-slash path, `""` when unknown.
    pub material_path: String,
    /// The Maya mapping name, for display.
    pub mapping_name: String,
    /// The `.materialgraph` the material instances, `""` when it names none.
    pub template: String,
    /// True when the `.material` asset was found in the TOC and parsed.
    pub resolved: bool,
    /// **The viewport's fallback signal.** True when this slot is a shipped
    /// placeholder (`gl_default_grid`, `gl_default`, `default_pink`), when the
    /// material is missing from the archives, or when every texture it binds is
    /// a placeholder. Draw these with the neutral debug colour instead of
    /// papering a pink grid over the level.
    pub placeholder: bool,
    pub textures: Vec<TextureRef>,
    /// **Additive.** Present only when the material instances the
    /// `gbl_rgb_blend` family, in which case one albedo out of `textures` is not
    /// what the surface shows: see [`BlendRecipe`]. `textures` keeps its old
    /// meaning either way, so a caller that ignores this field behaves exactly
    /// as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendRecipe>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMaterials {
    pub model_id: String,
    pub slots: Vec<MaterialSlot>,
}

// ---------------------------------------------------------------------------
// Slot semantics
// ---------------------------------------------------------------------------

/// Where a slot name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotName {
    /// From the material's own template sampler table (or a sampler hash some
    /// other material declared).
    Declared,
    /// From the bound texture's own filename suffix.
    Guessed,
}

/// Filename suffix to semantic. Returns `None` for a suffix that says nothing.
fn semantic_of_path(path: &str) -> Option<&'static str> {
    let file = path.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    let stem = file.strip_suffix(".texture").unwrap_or(&file);
    let suffix = stem.rsplit('_').next()?;
    Some(match suffix {
        "c" | "col" | "color" | "colour" | "albedo" | "basecolor" | "d" | "diff" => "albedo",
        "n" | "nrm" | "norm" | "normal" => "normal",
        "g" | "gloss" | "glossiness" | "r" | "rough" | "roughness" => "roughness",
        "m" | "mp" | "mask" | "masks" => "mask",
        "h" | "height" => "height",
        "ao" | "occl" => "ao",
        "id" | "ids" => "id",
        "e" | "emiss" | "emissive" | "glow" => "emissive",
        "det" | "detail" => "detail",
        _ => return None,
    })
}

/// The few slot names recovered by dictionary attack live in
/// [`material_names`]; fold them into the same vocabulary.
fn semantic_of_known_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    Some(if n.starts_with("basemap") {
        "albedo"
    } else if n.starts_with("normalmap") {
        "normal"
    } else if n.starts_with("glossmap") {
        "roughness"
    } else if n.starts_with("maskmap") {
        "mask"
    } else {
        return None;
    })
}

/// Last resort: the texture's own `content_type` tag. Measured over a busy art
/// zone it is a clean discriminator — every `_n` map carries `0x02` and nothing
/// else does, every `_c` carries `0x01` — so it rescues the bindings whose file
/// name ends in something uninformative (`trim_master_001`, `grunge_001`).
fn semantic_of_content(info: Option<TexInfo>) -> Option<&'static str> {
    match info?.content_type {
        texture_serve::CONTENT_COLOR => Some("albedo"),
        texture_serve::CONTENT_NORMAL => Some("normal"),
        _ => None,
    }
}

/// Process-wide `sampler name_hash -> semantic`, learned from every template
/// sampler table this process has parsed.
///
/// A `.material` embeds its own template's table, so this fills itself as
/// materials are read; it exists so that the handful of materials that ship
/// *without* the section still resolve, and so a second model reusing a shared
/// sampler hash never has to guess.
fn semantic_table() -> &'static DashMap<u32, &'static str> {
    static TABLE: OnceLock<DashMap<u32, &'static str>> = OnceLock::new();
    TABLE.get_or_init(DashMap::new)
}

fn learn_template(template: &MaterialTemplate) {
    for s in &template.samplers {
        if let Some(sem) = semantic_of_path(&s.default_path) {
            semantic_table().entry(s.name_hash).or_insert(sem);
        }
    }
}

/// Distinct sampler hashes the semantic table currently knows, for the probe.
pub fn semantic_table_len() -> usize {
    semantic_table().len()
}

// ---------------------------------------------------------------------------
// Placeholders
// ---------------------------------------------------------------------------

/// Placeholder `.material` paths — the ones the game itself renders as a pink
/// grid when an artist has not assigned anything yet.
pub fn is_placeholder_material(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("gl_default_grid") || p.contains("gl_default") || p.contains("default_pink")
}

/// Placeholder `.texture` paths — 2x2 pink, 16x16 white, flat black, the grid.
pub fn is_placeholder_texture(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("default_pink_shader")
        || p.contains("default_grid")
        || p.ends_with("mo_white_h.texture")
        || p.contains("/mo_black")
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

fn hex_id(id: u64) -> String {
    format!("{id:#018X}")
}

/// One material's textures plus the template it instances.
struct ResolvedMaterial {
    template: String,
    textures: Vec<TextureRef>,
    blend: Option<BlendRecipe>,
}

/// Counters the probe reports; every resolution bumps them.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResolveCounters {
    pub slots: usize,
    pub slots_resolved: usize,
    pub slots_placeholder: usize,
    pub bindings: usize,
    pub bindings_declared: usize,
    pub bindings_guessed: usize,
    pub bindings_unnamed: usize,
    pub textures_resolved: usize,
    pub textures_missing: usize,
}

fn counters() -> &'static Mutex<ResolveCounters> {
    static C: OnceLock<Mutex<ResolveCounters>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ResolveCounters::default()))
}

/// Read and zero the counters — call it around a measured pass.
pub fn take_counters() -> ResolveCounters {
    let mut g = counters().lock().unwrap();
    std::mem::take(&mut *g)
}

fn bump(f: impl FnOnce(&mut ResolveCounters)) {
    f(&mut counters().lock().unwrap());
}

/// Parse one `.material` and resolve every texture it binds.
fn resolve_material(index: &LevelIndex, material_id: u64) -> Result<ResolvedMaterial> {
    let raw = index.extract(material_id)?;
    let mat = MaterialFile::parse(&raw)?;
    let template_path = mat.template_path().unwrap_or_default();

    // The material embeds a copy of its template's sampler table; learn from it
    // whether or not this particular material binds every slot.
    let template = MaterialTemplate::from_dat1(&mat.dat1).unwrap_or_default();
    learn_template(&template);

    let serialized = match mat.dat1.get_section_data(TAG_MATERIAL_SERIALIZED) {
        Some(sec) => MaterialSerialized::parse(sec)?,
        None => MaterialSerialized::default(),
    };

    let mut textures = Vec::with_capacity(serialized.samplers.len());
    for s in &serialized.samplers {
        let path = crc64::normalize_path(&s.path);

        let texture_id = if path.is_empty() {
            0
        } else {
            crc64::hash_raw(path.as_bytes())
        };
        let info: Option<TexInfo> = if texture_id == 0 {
            None
        } else {
            texture_serve::texture_info(index, texture_id)
        };

        // 1. this material's own template table, 2. anything another material
        // has declared for the same hash, 3. a recovered literal slot name,
        // 4. the bound texture's own suffix, 5. the texture header's own
        //    content-type tag.
        let (slot, source) = template
            .samplers
            .iter()
            .find(|t| t.name_hash == s.name_hash)
            .and_then(|t| semantic_of_path(&t.default_path))
            .map(|sem| (sem, SlotName::Declared))
            .or_else(|| {
                semantic_table()
                    .get(&s.name_hash)
                    .map(|sem| (*sem, SlotName::Declared))
            })
            .or_else(|| {
                material_names::resolve(s.name_hash)
                    .as_deref()
                    .and_then(semantic_of_known_name)
                    .map(|sem| (sem, SlotName::Declared))
            })
            .or_else(|| semantic_of_path(&path).map(|sem| (sem, SlotName::Guessed)))
            .or_else(|| semantic_of_content(info).map(|sem| (sem, SlotName::Guessed)))
            .unwrap_or(("other", SlotName::Guessed));

        let (w, h) = info.map(|i| i.native()).unwrap_or((0, 0));

        bump(|c| {
            c.bindings += 1;
            match (source, slot) {
                (_, "other") => c.bindings_unnamed += 1,
                (SlotName::Declared, _) => c.bindings_declared += 1,
                (SlotName::Guessed, _) => c.bindings_guessed += 1,
            }
            if info.is_some() {
                c.textures_resolved += 1;
            } else {
                c.textures_missing += 1;
            }
        });

        textures.push(TextureRef {
            slot: slot.to_string(),
            texture_id: hex_id(texture_id),
            texture_path: path.clone(),
            width: w,
            height: h,
            format: info.map(|i| texture_serve::format_name(i.format)).unwrap_or_default(),
            named: source == SlotName::Declared,
            placeholder: is_placeholder_texture(&path),
            srgb: info.map(|i| texture_serve::is_srgb(i.format)).unwrap_or(false),
            resolved: info.is_some(),
        });
    }

    let blend = derive_blend(&template, &serialized, &template_path);

    Ok(ResolvedMaterial {
        template: template_path,
        textures,
        blend,
    })
}

/// Resolve every material slot of one model.
///
/// Never fails on a per-slot basis: a slot whose material is missing from the
/// archives comes back `resolved: false, placeholder: true` with no textures,
/// which is the viewport's cue to draw its neutral debug colour.
pub fn resolve_model(index: &LevelIndex, model_id: u64) -> Result<ModelMaterials> {
    let raw = index.extract(model_id)?;
    let model = level::parse_model_asset(&raw)?;

    let Some(section) = model.dat1.get_section_data(TAG_MATERIAL) else {
        return Ok(ModelMaterials {
            model_id: hex_id(model_id),
            slots: Vec::new(),
        });
    };
    let table = MaterialSection::parse(section)?;

    let mut slots = Vec::with_capacity(table.slots.len());
    for (i, slot) in table.slots.iter().enumerate() {
        let material_path = model
            .dat1
            .get_string(slot.path_offset as u32)
            .map(|s| crc64::normalize_path(&s))
            .unwrap_or_default();
        let mapping_name = model
            .dat1
            .get_string(slot.name_offset as u32)
            .unwrap_or_default();

        // The path is the identity: hashing it gives the TOC id directly. The
        // `ids` table is sorted by the mapping-name hash, not parallel to the
        // slot array, so it is consulted by name and only as a fallback.
        let by_path = if material_path.is_empty() {
            0
        } else {
            crc64::hash_raw(material_path.as_bytes())
        };
        let by_table = table
            .id_for_name(&mapping_name)
            .map(|e| e.asset_id)
            .unwrap_or(0);
        let material_id = if by_path != 0 && index.asset(by_path).is_some() {
            by_path
        } else if by_table != 0 && index.asset(by_table).is_some() {
            by_table
        } else if by_path != 0 {
            by_path
        } else {
            by_table
        };

        let known_placeholder = is_placeholder_material(&material_path);
        let resolved = material_id != 0 && index.asset(material_id).is_some();

        let (template, textures, blend) = if resolved && !known_placeholder {
            match resolve_material(index, material_id) {
                Ok(r) => (r.template, r.textures, r.blend),
                Err(e) => {
                    debug!("material {material_id:016X} ({material_path}): {e}");
                    (String::new(), Vec::new(), None)
                }
            }
        } else {
            (String::new(), Vec::new(), None)
        };

        // A slot that binds nothing usable is as good as a placeholder to the
        // viewport: 27% of shipped materials bind only placeholder textures.
        let all_placeholder = !textures.is_empty()
            && textures
                .iter()
                .all(|t| t.placeholder || !t.resolved);
        let placeholder =
            known_placeholder || !resolved || textures.is_empty() || all_placeholder;

        bump(|c| {
            c.slots += 1;
            if resolved {
                c.slots_resolved += 1;
            }
            if placeholder {
                c.slots_placeholder += 1;
            }
        });

        slots.push(MaterialSlot {
            index: i as u32,
            material_id: if material_id == 0 {
                "0x0".to_string()
            } else {
                hex_id(material_id)
            },
            material_path,
            mapping_name,
            template,
            resolved,
            placeholder,
            textures,
            blend,
        });
    }

    Ok(ModelMaterials {
        model_id: hex_id(model_id),
        slots,
    })
}

// ---------------------------------------------------------------------------
// Model LRU
// ---------------------------------------------------------------------------

struct ModelCache {
    map: DashMap<u64, Arc<ModelMaterials>>,
    order: Mutex<Vec<u64>>,
    hits: AtomicUsize,
}

fn model_cache() -> &'static ModelCache {
    static CACHE: OnceLock<ModelCache> = OnceLock::new();
    CACHE.get_or_init(|| ModelCache {
        map: DashMap::new(),
        order: Mutex::new(Vec::new()),
        hits: AtomicUsize::new(0),
    })
}

/// Resolved slots for a model, computed at most once per id.
///
/// Never fails: a model that cannot be extracted or parsed yields an empty slot
/// list rather than an error, so one bad asset does not abort a zone's loop.
pub fn model_materials(index: &LevelIndex, model_id: u64) -> Arc<ModelMaterials> {
    let cache = model_cache();
    if let Some(hit) = cache.map.get(&model_id) {
        cache.hits.fetch_add(1, Ordering::Relaxed);
        return Arc::clone(&hit);
    }

    let resolved = resolve_model(index, model_id).unwrap_or_else(|e| {
        debug!("model {model_id:016X} materials: {e}");
        ModelMaterials {
            model_id: hex_id(model_id),
            slots: Vec::new(),
        }
    });

    let arc = Arc::new(resolved);
    if cache.map.insert(model_id, Arc::clone(&arc)).is_none() {
        let mut order = cache.order.lock().unwrap();
        order.push(model_id);
        while order.len() > MODEL_CACHE_CAP {
            let victim = order.remove(0);
            cache.map.remove(&victim);
        }
    }
    arc
}

/// `(entries, hits)` of the resolved-model cache, for diagnostics.
pub fn model_cache_stats() -> (usize, usize) {
    let c = model_cache();
    (c.map.len(), c.hits.load(Ordering::Relaxed))
}

/// Parse an id as produced by [`hex_id`]; the `0x` prefix is optional.
pub fn parse_hex_id(s: &str) -> Result<u64> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(t, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id {s:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::material::{MaterialConstant, MaterialSampler};
    use crate::core::material_graph::{TemplateConstant, TemplateSampler};

    fn sampler(name_hash: u32, slot_index: u16, default_path: &str) -> TemplateSampler {
        TemplateSampler {
            name_hash,
            slot_index,
            user_exposed: 1,
            type_hash: 0x49A7_866B,
            default_path: default_path.into(),
        }
    }

    fn constant(name_hash: u32, values: &[f32]) -> TemplateConstant {
        TemplateConstant {
            name_hash,
            buffer_offset: 0,
            buffer_size: (values.len() * 4) as u16,
            default_values: values.to_vec(),
        }
    }

    /// The four-layer `gbl_rgb_blend` interface, trimmed to what the recipe
    /// reads: the splat, the shared normal and misc maps, the four layer
    /// colours, and each layer's tiling / scale / HSL.
    fn four_layer_template() -> MaterialTemplate {
        let pink = "textures/default_pink_shader/default_pink_mp.texture";
        MaterialTemplate {
            samplers: vec![
                sampler(
                    0x205D_ECC3,
                    0,
                    "textures/default_pink_shader/default_pink_shader_c.texture",
                ),
                sampler(
                    0x61D0_637F,
                    1,
                    "textures/default_pink_shader/default_pink_shader_n.texture",
                ),
                sampler(
                    0x3C65_1BCB,
                    2,
                    "textures/environment/global/masks/mo_gbl_masks/mo_white_h.texture",
                ),
                sampler(0xE4CF_0EAD, 3, pink),
                sampler(0x54C8_3B26, 4, pink),
                sampler(0x013D_D88D, 5, pink),
                sampler(0x9E34_A894, 6, pink),
            ],
            constants: vec![
                constant(0x2356_CD0C, &[1.0]),
                constant(0x1BD0_45A3, &[1.0]),
                constant(0xD1C7_B633, &[1.0]),
                constant(0xBBAB_D142, &[1.0]),
                constant(0x1D93_CDB7, &[1.0, 1.0]),
                constant(0x4BAF_5B9E, &[1.0, 1.0]),
                constant(0x62D7_112C, &[1.0, 1.0]),
                constant(0x4EE4_4C3D, &[0.5]),
                constant(0x3E1E_E9AB, &[0.5]),
                constant(0xD3DF_84B4, &[0.5]),
                constant(0x18D8_DA14, &[0.5]),
                constant(0x25B3_6543, &[0.5]),
                constant(0x7FF6_17E1, &[0.5]),
                constant(0x886E_717D, &[0.5]),
                constant(0x6A81_9148, &[0.5]),
                constant(0x92F0_4E42, &[0.5]),
                constant(0x679C_068F, &[0.5]),
                constant(0x33AF_D592, &[0.5]),
                constant(0x0F89_833E, &[0.5]),
            ],
            variations: Vec::new(),
        }
    }

    /// A real Nefarious City trim: steel on the base and on the red and green
    /// layers, rust on the blue one, and only the blue layer's HSL pushed off
    /// neutral.
    fn steel_and_rust() -> MaterialSerialized {
        let steel = "textures/sbs/metal/steel_001_c.texture";
        MaterialSerialized {
            constants: vec![
                MaterialConstant { name_hash: 0x679C_068F, values: vec![0.591] },
                MaterialConstant { name_hash: 0x33AF_D592, values: vec![0.945] },
                MaterialConstant { name_hash: 0x0F89_833E, values: vec![0.576] },
                MaterialConstant { name_hash: 0x62D7_112C, values: vec![2.0, 3.0] },
            ],
            samplers: vec![
                MaterialSampler {
                    name_hash: 0x205D_ECC3,
                    path: "environment/NeoNefarious/Architecture/NNC_arch_bldg_trim_01_id.texture"
                        .into(),
                },
                MaterialSampler {
                    name_hash: 0x61D0_637F,
                    path: "textures/sbs/macro/trim_master_001.texture".into(),
                },
                MaterialSampler { name_hash: 0x9E34_A894, path: steel.into() },
                MaterialSampler { name_hash: 0x54C8_3B26, path: steel.into() },
                MaterialSampler { name_hash: 0x013D_D88D, path: steel.into() },
                MaterialSampler {
                    name_hash: 0xE4CF_0EAD,
                    path: "textures/sbs/grunge/rust_001_c.texture".into(),
                },
            ],
            aux: Vec::new(),
        }
    }

    #[test]
    fn a_four_layer_blend_resolves_every_layer() {
        let r = derive_blend(
            &four_layer_template(),
            &steel_and_rust(),
            "materialgraph\\global\\gbl_rgb_blend\\gbl_rgb_blend.materialgraph",
        )
        .expect("recipe");

        assert_eq!(r.kind, "rgb_blend");
        assert_eq!(
            r.template,
            "materialgraph/global/gbl_rgb_blend/gbl_rgb_blend.materialgraph"
        );
        // The splat is the id mask, not the albedo-suffixed template default.
        assert!(r.mask_texture_path.ends_with("nnc_arch_bldg_trim_01_id.texture"));
        assert_eq!(r.normal_texture_path, "textures/sbs/macro/trim_master_001.texture");
        // mo_white_h is a shipped placeholder, so the shared misc slot is empty.
        assert_eq!(r.misc_texture_path, "");

        let ch: Vec<&str> = r.layers.iter().map(|l| l.channel.as_str()).collect();
        assert_eq!(ch, ["base", "r", "g", "b"]);
        assert!(r.layers.iter().all(|l| l.own_texture));
        assert!(r.layers[0].texture_path.ends_with("steel_001_c.texture"));
        // ONE COHERENT GREEN LAYER. This material overrides exactly three things
        // — the `E4CF0EAD` sampler, the `62D7112C` tiling and `Green_HSL_*` —
        // and the compiled shader reads all three as the layer driven by the
        // splat mask's GREEN channel. The table this test used to assert split
        // them across two layers: the texture and the tiling landed on blue
        // while the tint stayed on green, so the surface showed rust with no
        // tint next to tinted steel at the wrong tiling. Keeping the three
        // assertions adjacent is the regression guard.
        assert!(r.layers[2].texture_path.ends_with("rust_001_c.texture"));
        assert_eq!(r.layers[2].uv_scale, [2.0, 3.0]);
        assert_eq!(r.layers[2].hsl, [0.591, 0.945, 0.576]);
        assert_eq!(r.layers[0].hsl, [0.5, 0.5, 0.5]);
        assert_eq!(r.layers[1].hsl, [0.5, 0.5, 0.5]);
        assert_eq!(r.layers[3].hsl, [0.5, 0.5, 0.5]);
        assert_eq!(r.layers[1].uv_scale, [1.0, 1.0]);
        assert_eq!(r.layers[3].uv_scale, [1.0, 1.0]);
        // The base layer's tiling is a real slot now, so it resolves like any
        // other. `8D2566DE` is unset here, hence the template default.
        assert_eq!(r.layers[0].uv_scale, [1.0, 1.0]);
        // Only the three splat layers carry a blend pair and an invert.
        assert!(r.layers.iter().all(|l| l.blend == [1.0, 0.33] && l.invert == 0.0));
        // Every triple is named, so every layer's HSL is measured.
        assert!(r.layers.iter().all(|l| l.hsl_source == "measured"));
        // Nothing is dropped: every declared constant reaches params.
        assert_eq!(r.params.len(), four_layer_template().constants.len());
        assert_eq!(r.params["0x679C068F"], vec![0.591]);
    }

    #[test]
    fn a_layer_with_no_texture_of_its_own_falls_back_to_the_base() {
        let mut ser = steel_and_rust();
        ser.samplers.retain(|s| s.name_hash != 0xE4CF_0EAD);
        let r = derive_blend(&four_layer_template(), &ser, "x.materialgraph").expect("recipe");
        assert!(!r.layers[2].own_texture);
        assert_eq!(r.layers[2].texture_path, r.layers[0].texture_path);
    }

    #[test]
    fn a_three_layer_variant_yields_three_layers() {
        // A variant that drops the blue layer drops its sampler, its tiling, its
        // normal intensity and `Blue_HSL_*` together — and blue's sampler is
        // `013DD88D`, per the shader.
        let mut t = four_layer_template();
        t.samplers.retain(|s| s.name_hash != 0x013D_D88D);
        t.constants.retain(|c| {
            !matches!(
                c.name_hash,
                0x4EE4_4C3D | 0x3E1E_E9AB | 0xD3DF_84B4 | 0x4BAF_5B9E | 0xD1C7_B633
            )
        });
        let r = derive_blend(&t, &steel_and_rust(), "x.materialgraph").expect("recipe");
        let ch: Vec<&str> = r.layers.iter().map(|l| l.channel.as_str()).collect();
        assert_eq!(ch, ["base", "r", "g"]);
    }

    #[test]
    fn a_material_outside_the_family_has_no_recipe() {
        let t = MaterialTemplate {
            samplers: vec![sampler(0xABDA_D780, 0, "a/b_c.texture")],
            constants: Vec::new(),
            variations: Vec::new(),
        };
        assert!(derive_blend(&t, &MaterialSerialized::default(), "basic.materialgraph").is_none());
    }

    #[test]
    fn suffixes_name_the_semantic() {
        assert_eq!(
            semantic_of_path("textures/default_pink_shader/default_pink_shader_c.texture"),
            Some("albedo")
        );
        assert_eq!(semantic_of_path("a/b/solar_panels_002_n.texture"), Some("normal"));
        assert_eq!(semantic_of_path("a/b/solar_panels_002_g.texture"), Some("roughness"));
        assert_eq!(semantic_of_path("a/b/default_pink_mp.texture"), Some("mask"));
        assert_eq!(semantic_of_path("a/b/mo_white_h.texture"), Some("height"));
        assert_eq!(semantic_of_path("a/b/meg_arch_trim_02_id.texture"), Some("id"));
        assert_eq!(semantic_of_path("a/b/trim_master_001.texture"), None);
    }

    #[test]
    fn placeholders_are_recognised_on_both_sides() {
        assert!(is_placeholder_material(
            "materials/gl_default_grid/grid_light_pink.material"
        ));
        assert!(is_placeholder_texture(
            "textures/default_pink_shader/default_pink_shader_c.texture"
        ));
        assert!(is_placeholder_texture(
            "textures/environment/global/masks/mo_gbl_masks/mo_white_h.texture"
        ));
        assert!(!is_placeholder_material(
            "materials/environment/megalopolis/meg_arch_steel_trim_001.material"
        ));
        assert!(!is_placeholder_texture("textures/sbs/metal/steel_001_c.texture"));
    }

    #[test]
    fn doubled_separators_collapse_to_the_shipped_id() {
        // Authored paths sometimes carry `\\`; only the collapsed single-slash
        // form is in the TOC, so both spellings must hash the same.
        let a = crc64::normalize_path(r"textures\\default_pink_shader\\x.texture");
        let b = crc64::normalize_path("textures/default_pink_shader/x.texture");
        assert_eq!(a, b);
        assert_eq!(crc64::hash_raw(a.as_bytes()), crc64::hash_raw(b.as_bytes()));
    }

    #[test]
    fn hex_ids_round_trip() {
        let id = 0x8123_4567_89AB_CDEFu64;
        assert_eq!(parse_hex_id(&hex_id(id)).unwrap(), id);
        assert_eq!(parse_hex_id("0x0").unwrap(), 0);
    }

    #[test]
    fn known_slot_names_fold_into_the_vocabulary() {
        assert_eq!(semantic_of_known_name("BaseMap2D_Texture"), Some("albedo"));
        assert_eq!(semantic_of_known_name("NormalMap2D_Texture"), Some("normal"));
        assert_eq!(semantic_of_known_name("Glossiness"), None);
    }
}
