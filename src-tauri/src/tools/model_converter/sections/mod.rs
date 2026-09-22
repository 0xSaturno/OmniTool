pub mod geo;
pub mod meshes;
pub mod skin;
pub mod joints;
pub mod look;
pub mod built;
pub mod skeleton;
pub mod looks;
pub mod morph;
pub mod dynamics;
pub mod splines;
pub mod physics;
pub mod render;

pub use geo::*;
pub use meshes::*;
pub use skin::*;
pub use joints::*;
pub use look::*;
pub use built::*;
pub use skeleton::*;
pub use looks::*;
pub use morph::*;
pub use dynamics::*;
pub use splines::*;
pub use physics::*;
pub use render::*;

/// Display name of a `.model` section tag, and whether the name string itself is recovered (crc matches) rather than inferred from content.
pub fn section_name(tag: u32) -> Option<(&'static str, bool)> {
    Some(match tag {
        0x283D0383 => ("Model Built", true),
        0xA98BE69B => ("Model Std Vert", true),
        0x16F3BA18 => ("Model Tex Vert", true),
        0x6B855EED => ("Model UV1 Vert", true),
        0xCCBAFF15 => ("Model GPU Skin", true),
        0xDCA379A2 => ("Model Skin Data", true),
        0xC61B1FF5 => ("Model Skin Batch", true),
        0x5240C82B => ("Model Skin Joint Remap", true),
        0x0859863D => ("Model Index", true),
        0x78D9CBDE => ("Model Subset", true),
        0x3250BB80 => ("Model Material", true),
        0xDCC88A19 => ("Model Bind Pose", true),
        0x90CDB60C => ("Model Joint Hierarchy", true),
        0x15DF9D3B => ("Model Joint", true),
        0x0AD3A708 => ("Model Joint Bspheres", true),
        0xEE31971C => ("Model Joint Lookup", true),
        0x9F614FAB => ("Model Locator", true),
        0x731CBC2E => ("Model Locator Lookup", true),
        0xC5354B60 => ("Model Mirror Ids", true),
        0x5CBA9DE9 => ("Model Col Vert", true),
        0xEFD92E68 => ("Model Physics Data", true),
        0x5E709570 => ("Model Anim Morph Data", true),
        0xA600C108 => ("Model Anim Morph Indices", true),
        0x380A5744 => ("Model Anim Morph Info", true),
        0xADD1CBD3 => ("Model Anim Dynamics Def", true),
        0xCD903318 => ("Model Anim Geom Info", true),
        0x3F70F60D => ("Model Anim Geom Particles", true),
        0x42349A17 => ("Model Anim Ziva Info", true),
        0x244E5823 => ("Model Anim Ziva Data", true),
        0x06EB7EFC => ("Model Look", true),
        0x811902D7 => ("Model Look Built", true),
        0x4CCEA4AD => ("Model Look Group", true),
        0xDF9FDF12 => ("Model Look BVH Info", true),
        0xB7380E8C => ("Model Leaf Ids", true),
        0x27CA5246 => ("Model Splines", true),
        0x3C9DABDF => ("Model Spline Subsets", true),
        0xBB7303D5 => ("Model Spline Skin Binding", true),
        0xB25B3163 => ("Model Splines CVs", true),
        0xBCE86B01 => ("Model Render Overrides", true),
        0x00823787 => ("Model Texture Overrides", true),
        0x5796FEF6 => ("Model Collision Index Data", true),
        0xF4CB2F37 => ("Model Collision Vertex Data", true),
        0x707F1B58 => ("Ragdoll Meta Data", false),
        0x9A434B29 => ("IK Setup", false),
        0x5A39FAB7 => ("Cloth Meta Data", false),
        0x7CA37DA0 => ("Ambient Shadow Prims", false),
        0x665DA362 => ("Perf Profile Max LoD", false),
        _ => return None,
    })
}
