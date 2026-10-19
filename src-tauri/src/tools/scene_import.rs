//! glTF scene import: one model per mesh, one zone instance per node that uses it, and the
//! zone's collision sidecar (`<zone>.obj`, world space) that rivet_hook cooks when the zone loads.
//!
//! Collision comes from the nodes named `<thing>_col` when the scene has any; they are not drawn.
//! Otherwise it comes from every drawn node not tagged `rcra_collision = "none"`.
//!
//! A glTF material with a base colour image gets a material and a texture of its own, cloned from
//! the surface donors; its alpha mode, double-sidedness, roughness (as 1 - glossiness) and base
//! colour factor (tint and opacity) carry over, and `rcra_no_shadow = true` in its extras stops
//! it casting shadows. `rcra_textures` in its extras binds ready-made `.texture` files (base colour,
//! normal, gloss) instead, copied into the mod as they are; see `bound_textures`.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use image::RgbaImage;

use crate::core::error::{Result, ToolkitError};
use crate::core::material::{AlphaKind, MaterialFile, Surface};
use crate::core::zone::{ModelPlacement, Zone, ZoneEdits};
use crate::tools::texture_converter::texture_from_rgba;
use crate::tools::model_converter::gltf_import::{import_gltf, material_slot_plan, strip_blender_suffix, ImportOptions};
use crate::tools::model_converter::gltf_reader::{extras_json, parse_gltf, GltfMesh, GltfModel};
use crate::tools::model_converter::model::ModelFile;
use crate::tools::model_converter::sections::built::{Built, TAG_BUILT};

/// Column-major, as glTF stores it: `m[column][row]`.
type Mat4 = [[f32; 4]; 4];

const IDENTITY: Mat4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
/// rivet_hook's default `[collision] max_triangles`.
const DEFAULT_COLLISION_TRIANGLES: usize = 2048;
const COLLISION_RANGE: f32 = 4096.0;

#[derive(Debug, Default)]
pub struct SceneOutput {
    /// Asset path and contents of every file of the mod folder.
    pub files: Vec<(String, Vec<u8>)>,
    pub models: usize,
    pub materials: usize,
    pub instances: usize,
    pub collision_triangles: usize,
    /// Message and how many times it came up.
    pub warnings: BTreeMap<String, usize>,
}

struct Placed {
    node: usize,
    mesh: usize,
    world: Mat4,
    collision_only: bool,
    no_collision: bool,
}

fn mul(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn transform(m: Mat4, p: (f32, f32, f32)) -> [f32; 3] {
    [0, 1, 2].map(|r| m[0][r] * p.0 + m[1][r] * p.1 + m[2][r] * p.2 + m[3][r])
}

fn mirrored(m: Mat4) -> bool {
    let (x, y, z) = (m[0], m[1], m[2]);
    x[0] * (y[1] * z[2] - y[2] * z[1]) - x[1] * (y[0] * z[2] - y[2] * z[0]) + x[2] * (y[0] * z[1] - y[1] * z[0]) < 0.0
}

/// rivet_hook's collision builder rejects the whole mesh over one triangle with two corners it
/// would weld (within 1 cm) or all three in a line; same test, same f32 order as collision.cpp.
fn hook_rejects(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> bool {
    const SHORTEST2: f32 = 0.01 * 0.01;
    let d = |p: [f32; 3], q: [f32; 3]| [0, 1, 2].map(|k| q[k] - p[k]);
    let len2 = |v: [f32; 3]| v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    let (ab, ac, bc) = (d(a, b), d(a, c), d(b, c));
    let cross = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]];
    let (ab2, ac2) = (len2(ab), len2(ac));
    ab2 <= SHORTEST2 || ac2 <= SHORTEST2 || len2(bc) <= SHORTEST2 || len2(cross) <= 1.0e-8 * ab2 * ac2
}

fn asset_name(name: &str) -> String {
    name.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn rgba(image: &gltf::image::Data) -> Option<RgbaImage> {
    use gltf::image::Format;
    let pixels = match image.format {
        Format::R8G8B8A8 => image.pixels.clone(),
        Format::R8G8B8 => image.pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    RgbaImage::from_raw(image.width, image.height, pixels)
}

/// Sampler slots a glTF material can bind ready-made textures to, with their name hashes.
const TEXTURE_SLOTS: [(&str, u32); 3] = [
    ("BaseMap2D_Texture", 0xABDA_D780),
    ("NormalMap2D_Texture", 0x7AE3_4E7A),
    ("GlossMap2D_Texture", 0x1576_D848),
];

/// Ready-made `.texture` files a material names in its `rcra_textures` extras,
/// `{ "<slot>": { "asset": "<path under model_dir>", "file": "<local .texture>" } }`, added to the
/// mod once each. A file's HD part (`<file stem>.hd.texture`) rides along as `<asset>.stream`, the
/// name rivet_hook's loader reads a texture stream from. Returns `(sampler hash, asset path)`.
fn bound_textures(m: &gltf::Material, dir: &str, out: &mut SceneOutput) -> Vec<(u32, String)> {
    let Some(map) = extras_json(m.extras()).and_then(|v| v.get("rcra_textures").cloned()) else { return Vec::new() };
    let mut bound = Vec::new();
    for (slot, hash) in TEXTURE_SLOTS {
        let entry = &map[slot];
        let (Some(asset), Some(file)) = (entry["asset"].as_str(), entry["file"].as_str()) else { continue };
        let asset = format!("{dir}/{}", asset.replace('\\', "/").to_ascii_lowercase());
        if !out.files.iter().any(|(p, _)| *p == asset) {
            let Ok(bytes) = std::fs::read(file) else {
                *out.warnings.entry(format!("texture file '{file}' could not be read")).or_default() += 1;
                continue;
            };
            if let Ok(hd) = std::fs::read(format!("{}.hd.texture", file.trim_end_matches(".texture"))) {
                out.files.push((format!("{asset}.stream"), hd));
            }
            out.files.push((asset.clone(), bytes));
        }
        bound.push((hash, asset));
    }
    bound
}

/// Gives every glTF material that has a base colour (image or `rcra_textures`) a material of its own
/// under `dir`, cloned from the `surface` donors (`.material`, `.texture`), and maps it in `opts`.
fn clone_materials(
    document: &gltf::Document,
    images: &[gltf::image::Data],
    dir: &str,
    (material, texture): (&[u8], &[u8]),
    opts: &mut ImportOptions,
    out: &mut SceneOutput,
) -> Result<()> {
    for m in document.materials() {
        let Some(name) = m.name() else { continue };
        if opts.materials.keys().any(|k| k.eq_ignore_ascii_case(name)) {
            continue;
        }
        // A ready-made base colour texture replaces the one built from the glTF image.
        let bound = bound_textures(&m, dir, out);
        let base_file = bound.iter().find(|(h, _)| *h == TEXTURE_SLOTS[0].1).map(|(_, p)| p.clone());
        let pixels = match (&base_file, m.pbr_metallic_roughness().base_color_texture()) {
            (Some(_), _) => None,
            (None, Some(info)) => match images.get(info.texture().source().index()).and_then(rgba) {
                Some(p) => Some(p),
                None => {
                    *out.warnings.entry(format!("material '{name}': its image is not 8-bit RGB or RGBA")).or_default() += 1;
                    continue;
                }
            },
            (None, None) => continue,
        };
        let alpha = match m.alpha_mode() {
            gltf::material::AlphaMode::Opaque => AlphaKind::Opaque,
            gltf::material::AlphaMode::Mask => AlphaKind::Cutout(m.alpha_cutoff().unwrap_or(0.5)),
            gltf::material::AlphaMode::Blend => AlphaKind::Blend,
        };
        let base = format!("{dir}/{}", asset_name(name));
        let mut file = MaterialFile::parse(material)?;
        let [r, g, b, opacity] = m.pbr_metallic_roughness().base_color_factor();
        let look = Surface {
            alpha,
            double_sided: m.double_sided(),
            glossiness: 1.0 - m.pbr_metallic_roughness().roughness_factor(),
            tint: [r, g, b],
            opacity,
            cast_shadows: !extras_json(m.extras()).is_some_and(|v| v.get("rcra_no_shadow").and_then(|x| x.as_bool()) == Some(true)),
        };
        file.retexture(base_file.as_deref().unwrap_or(&format!("{base}.texture")), &look)?;
        for (hash, asset) in bound.iter().filter(|(h, _)| *h != TEXTURE_SLOTS[0].1) {
            file.set_sampler(*hash, asset)?;
        }
        out.files.push((format!("{base}.material"), file.save()));
        if let Some(pixels) = pixels {
            out.files.push((format!("{base}.texture"), texture_from_rgba(texture, &pixels).map_err(ToolkitError::Parse)?));
        }
        opts.materials.insert(name.to_string(), format!("{base}.material"));
        out.materials += 1;
    }
    Ok(())
}

/// Points primitives whose material has no slot and no `.material` path at the donor's first slot.
fn fallback_materials(model: &ModelFile, group: &mut GltfModel, opts: &ImportOptions, out: &mut SceneOutput) -> Result<()> {
    let plan = material_slot_plan(model, group, opts)?;
    let fallback = plan.existing.first().map(|s| s.name.clone()).ok_or_else(|| ToolkitError::Parse("the donor model has no material slot".into()))?;
    let unmapped: Vec<&str> = plan.new_slots.iter().filter(|s| s.path.is_none()).map(|s| s.name.as_str()).collect();
    for p in &mut group.meshes {
        if p.material.as_deref().map_or(true, |n| unmapped.contains(&n)) {
            p.material = Some(fallback.clone());
            *out.warnings.entry(format!("primitive whose material has no .material path; using the donor's '{fallback}'")).or_default() += 1;
        }
    }
    Ok(())
}

/// Builds the mod files for one model: `glb`'s meshes replace `donor`'s look geometry, skin weights
/// included (joints are matched to the donor's by name, so name them as the donor does), saved at
/// `model_path`. Materials are made as `import_scene` makes them, in the model's folder.
pub fn import_model(glb: &str, donor: &[u8], model_path: &str, surface: Option<(&[u8], &[u8])>, opts: &ImportOptions) -> Result<SceneOutput> {
    let mut out = SceneOutput::default();
    let (document, _, images) = gltf::import(glb).map_err(|e| ToolkitError::Parse(e.to_string()))?;
    let path = model_path.replace('\\', "/");
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut opts = opts.clone();
    if let Some(surface) = surface {
        clone_materials(&document, &images, dir, surface, &mut opts, &mut out)?;
    }
    let mut gltf = parse_gltf(glb)?;
    let mut model = ModelFile::parse(donor)?;
    fallback_materials(&model, &mut gltf, &opts, &mut out)?;
    for w in import_gltf(&mut model, &gltf, &opts)?.warnings {
        *out.warnings.entry(w).or_default() += 1;
    }
    out.files.push((path, model.save()));
    out.models = 1;
    Ok(out)
}

/// Builds the mod files for `glb`: models cloned from `donor` under `model_dir`, `host_zone` with
/// the instances added at `zone_path`, and the collision sidecar next to it. `surface` is the
/// donor `.material` (on `basic_normal_gloss`) and `.texture` for textured glTF materials.
pub fn import_scene(
    glb: &str,
    donor: &[u8],
    host_zone: &[u8],
    zone_path: &str,
    model_dir: &str,
    surface: Option<(&[u8], &[u8])>,
    opts: &ImportOptions,
) -> Result<SceneOutput> {
    let mut out = SceneOutput::default();
    let (document, _, images) = gltf::import(glb).map_err(|e| ToolkitError::Parse(e.to_string()))?;
    let dir = model_dir.replace('\\', "/");
    let dir = dir.trim_end_matches('/');

    let mut opts = opts.clone();
    if let Some(surface) = surface {
        clone_materials(&document, &images, dir, surface, &mut opts, &mut out)?;
    }
    let opts = &opts;

    // `parse_gltf` yields one entry per primitive in document order; regroup them by mesh.
    let mut flat = parse_gltf(glb)?.meshes.into_iter();
    let groups: Vec<Vec<GltfMesh>> = document.meshes().map(|m| flat.by_ref().take(m.primitives().count()).collect()).collect();

    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or_else(|| ToolkitError::Parse("the glTF has no scene".into()))?;
    let tagged_out = |x: &gltf::json::Extras| {
        extras_json(x).is_some_and(|v| v.get("rcra_collision").and_then(|c| c.as_str()).is_some_and(|c| c.eq_ignore_ascii_case("none")))
    };
    let mut placed: Vec<Placed> = Vec::new();
    let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = mul(parent, node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            let name = node.name().or(mesh.name()).unwrap_or_default();
            placed.push(Placed {
                node: node.index(),
                mesh: mesh.index(),
                world,
                collision_only: strip_blender_suffix(name).to_ascii_lowercase().ends_with("_col"),
                no_collision: tagged_out(node.extras()) || tagged_out(mesh.extras()),
            });
        }
        stack.extend(node.children().map(|c| (c, world)));
    }
    placed.sort_by_key(|p| p.node);

    // Models: one per mesh that a drawn node uses.
    let mut paths: Vec<Option<(String, Built)>> = vec![None; groups.len()];
    let mut names: HashSet<String> = HashSet::new();
    for (mi, mesh) in document.meshes().enumerate() {
        if !placed.iter().any(|p| p.mesh == mi && !p.collision_only) {
            continue;
        }
        if groups[mi].iter().all(|p| p.faces.is_empty()) {
            *out.warnings.entry(format!("mesh '{}' has no triangles; its nodes are skipped", mesh.name().unwrap_or_default())).or_default() += 1;
            continue;
        }
        let mut model = ModelFile::parse(donor)?;
        let mut group = GltfModel { bones: Vec::new(), meshes: groups[mi].clone(), looks: Vec::new() };

        fallback_materials(&model, &mut group, opts, &mut out)?;

        for w in import_gltf(&mut model, &group, opts)?.warnings {
            *out.warnings.entry(w).or_default() += 1;
        }
        let bytes = model.save();
        let built = model.dat1.get_section_data(TAG_BUILT).and_then(Built::parse).ok_or_else(|| ToolkitError::Parse("the donor model has no Built section".into()))?;
        let mut name = asset_name(mesh.name().unwrap_or_default());
        if name.is_empty() || names.contains(&name) {
            name = format!("{name}_{mi}");
        }
        names.insert(name.clone());
        let path = format!("{dir}/{name}.model");
        out.files.push((path.clone(), bytes));
        paths[mi] = Some((path, built));
        out.models += 1;
    }

    // Zone: every slot first, since new models renumber the ones behind them.
    let mut zone = Zone::parse(host_zone)?;
    zone.add_models(&paths.iter().flatten().map(|(p, _)| p.clone()).collect::<Vec<_>>())?;
    let mut placements = Vec::new();
    for p in placed.iter().filter(|p| !p.collision_only) {
        let Some((path, built)) = &paths[p.mesh] else { continue };
        let (w, c) = (p.world, built.bsphere_center);
        placements.push(ModelPlacement {
            model: zone.model_slot(path)?,
            axes: [0, 1, 2].map(|k| [w[k][0], w[k][1], w[k][2]]),
            position: [w[3][0], w[3][1], w[3][2]],
            bsphere: [c[0], c[1], c[2], built.bsphere_radius],
            half_extents: built.aabb_extents,
        });
    }
    zone.add_model_instances(&placements)?;
    out.instances = placements.len();
    out.files.push((zone_path.to_string(), zone.save(&ZoneEdits::default())?));

    // Collision sidecar.
    let explicit = placed.iter().any(|p| p.collision_only);
    let (mut obj, mut base, mut degenerate, mut far) = (String::new(), 1usize, 0usize, false);
    for p in placed.iter().filter(|p| if explicit { p.collision_only } else { !p.no_collision }) {
        let flip = mirrored(p.world);
        for prim in &groups[p.mesh] {
            let pos: Vec<[f32; 3]> = prim.vertexes.iter().map(|v| transform(p.world, v.position)).collect();
            for v in &pos {
                far |= v.iter().any(|c| c.abs() > COLLISION_RANGE);
                let _ = writeln!(obj, "v {} {} {}", v[0], v[1], v[2]);
            }
            // `faces` are stored reversed from glTF order.
            for &(a, b, c) in &prim.faces {
                let (a, b, c) = if flip { (a, b, c) } else { (c, b, a) };
                let rejected = match (pos.get(a as usize), pos.get(b as usize), pos.get(c as usize)) {
                    (Some(&pa), Some(&pb), Some(&pc)) => hook_rejects(pa, pb, pc),
                    _ => true,
                };
                if rejected {
                    degenerate += 1;
                    continue;
                }
                let _ = writeln!(obj, "f {} {} {}", base + a as usize, base + b as usize, base + c as usize);
                out.collision_triangles += 1;
            }
            base += pos.len();
        }
    }
    if out.collision_triangles > 0 {
        out.files.push((format!("{zone_path}.obj"), obj.into_bytes()));
    }
    let mut warnings = Vec::new();
    if degenerate > 0 {
        warnings.push(format!("{degenerate} degenerate collision triangles left out (corners under 1 cm apart or in a line)"));
    }
    if far {
        warnings.push(format!("collision reaches past ±{COLLISION_RANGE}, the range the hook accepts"));
    }
    if out.collision_triangles > DEFAULT_COLLISION_TRIANGLES {
        warnings.push(format!(
            "{} collision triangles; the hook takes {DEFAULT_COLLISION_TRIANGLES} unless [collision] max_triangles is raised (65536 at most)",
            out.collision_triangles
        ));
    }
    for w in warnings {
        *out.warnings.entry(w).or_default() += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_transforms_compose_parent_first() {
        let mut parent = IDENTITY;
        parent[0][0] = 2.0;
        parent[3] = [10.0, 0.0, 0.0, 1.0];
        let mut child = IDENTITY;
        child[3] = [1.0, 2.0, 3.0, 1.0];
        assert_eq!(transform(mul(parent, child), (1.0, 0.0, 0.0)), [14.0, 2.0, 3.0]);
        assert!(!mirrored(parent));
        parent[1][1] = -1.0;
        assert!(mirrored(parent));
    }

    #[test]
    fn rejects_what_the_hook_rejects() {
        let (o, x, y) = ([0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert!(!hook_rejects(o, x, y));
        assert!(hook_rejects(o, x, [0.0, 0.005, 0.0])); // a and c within 1 cm
        assert!(hook_rejects(o, x, [1.0, 0.005, 0.0])); // b and c within 1 cm: a bevel sliver
        assert!(hook_rejects(o, x, [2.0, 1.0e-6, 0.0])); // all three in a line
    }
}
