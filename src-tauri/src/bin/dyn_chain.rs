//! Anim-dynamics (jiggle) chain authoring on models that already have a Model Anim Dynamics Def.
//!
//! usage:
//!   dyn_chain list <model>
//!   dyn_chain retune <in> <out> <chain> [params]
//!   dyn_chain regen <in> <out> <chain> [--joints a,b,..] [--donor <model> [--donor-chain <name>]] [params]
//!   dyn_chain drop <in> <out> <chain>...
//!   dyn_chain repose <in> <out> <joint>=x,y,z...
//!   dyn_chain flag <in> <out>
//!   dyn_chain verify <model>...
//!
//! params: --type simple|cone|half|limited  --mass m  --damping d  --gravity y|x,y,z
//!         --cone outer[,end] (degrees)  --limits negY,posY,negZ,posZ
//!
//! retune edits one chain in place. regen, drop and repose rebuild every chain from its recovered
//! params, so they need a model whose chains all have a free end. regen takes its params from the
//! existing chain (or the donor's), then applies the options; --donor maps the donor chain's joints
//! onto this model by name. repose moves joints to new model-space bind positions. flag sets Built
//! flag bit 31. verify round-trips each section and regenerates every untethered chain.

use omnitool_lib::tools::model_converter::model::ModelFile;
use omnitool_lib::tools::model_converter::sections::*;

type R<T> = Result<T, String>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> R<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    if cmd == "list" {
        return list(&load(arg(args, 1)?)?);
    }
    if cmd == "verify" {
        return verify(&args[1..]);
    }
    let mut m = load(arg(args, 1)?)?;
    let out = arg(args, 2)?;
    let rest = &args[3..];
    match cmd {
        "retune" => retune(&mut m, rest)?,
        "regen" => regen(&mut m, rest)?,
        "drop" => drop_chains(&mut m, rest)?,
        "repose" => repose(&mut m, rest)?,
        "flag" => set_flag(&mut m)?,
        _ => return Err("usage: dyn_chain list|retune|regen|drop|repose|flag|verify ... (see the file header)".into()),
    }
    std::fs::write(out, m.save()).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}");
    list(&m)
}

fn arg(args: &[String], i: usize) -> R<&String> {
    args.get(i).ok_or_else(|| "missing argument (see the file header for usage)".to_string())
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn load(path: &str) -> R<ModelFile> {
    ModelFile::parse(&std::fs::read(path).map_err(|e| format!("{path}: {e}"))?).map_err(|e| format!("{path}: {e}"))
}

fn section(m: &ModelFile, tag: u32) -> R<&[u8]> {
    m.dat1.get_section_data(tag).ok_or_else(|| format!("model has no {} section", section_name(tag).map_or("?", |n| n.0)))
}

fn def(m: &ModelFile) -> R<AnimDynamicsDef> {
    AnimDynamicsDef::parse(section(m, TAG_ANIM_DYNAMICS_DEF)?).map_err(s)
}

fn put(m: &mut ModelFile, d: &AnimDynamicsDef) -> R<()> {
    m.dat1.set_section_data(TAG_ANIM_DYNAMICS_DEF, d.to_bytes()).map_err(s)
}

fn joint_names(m: &ModelFile) -> R<Vec<String>> {
    let joints = Joint::parse_all(section(m, TAG_JOINTS)?).map_err(s)?;
    Ok(joints.iter().map(|j| m.dat1.get_string(j.string_offset).unwrap_or_default()).collect())
}

fn joint_index(names: &[String], name: &str) -> R<u16> {
    names.iter().position(|n| n == name).map(|i| i as u16).ok_or_else(|| format!("no joint {name}"))
}

fn chain_name(m: &ModelFile, c: &DynamicsChain) -> String {
    m.dat1.get_string(c.name_offset).unwrap_or_default()
}

fn chain_index(m: &ModelFile, d: &AnimDynamicsDef, name: &str) -> R<usize> {
    d.chains.iter().position(|c| chain_name(m, c) == name).ok_or_else(|| format!("no chain {name}"))
}

fn flag_set(m: &ModelFile) -> bool {
    m.dat1.get_section_data(TAG_BUILT).and_then(|b| b.get(0x28..0x2C)).is_some_and(|f| f[3] & 0x80 != 0)
}

fn list(m: &ModelFile) -> R<()> {
    let d = def(m)?;
    let names = joint_names(m)?;
    println!("{} chains, Built flag 31 {}", d.chains.len(), if flag_set(m) { "set" } else { "CLEAR" });
    for (i, c) in d.chains.iter().enumerate() {
        let (joints, p) = d.recover(i).unwrap_or_default();
        println!(
            "{i:2} {:<28} type {} links {:2} mass {:.4} damping {:.3} gravity {:?} cone {:.2} -> {:.2} limits {:?} tethers {:?}",
            chain_name(m, c),
            c.constraint_type,
            c.links.1,
            p.mass,
            p.damping,
            p.gravity,
            p.cone_outer,
            p.cone_outer_end,
            p.limit_scales,
            c.tether_types
        );
        println!("     {}", joints.iter().map(|&j| names.get(j as usize).map_or("?", String::as_str)).collect::<Vec<_>>().join(" > "));
    }
    Ok(())
}

fn nums(v: &str) -> R<Vec<f64>> {
    v.split(',').map(|x| x.trim().parse::<f64>().map_err(|_| format!("bad number {x}"))).collect()
}

/// Applies the param options onto `p`; errors on anything else.
fn apply_params(args: &[String], p: &mut ChainParams) -> R<()> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = nums_or_word(it.next().ok_or_else(|| format!("{a} needs a value"))?);
        match (a.as_str(), v) {
            ("--type", Err(w)) => {
                p.constraint_type = match w.as_str() {
                    "simple" => dynamics_constraint::SIMPLE,
                    "cone" => dynamics_constraint::CONE,
                    "limited" => dynamics_constraint::LIMITED_CONE,
                    "half" => {
                        p.limit_scales[2] = 0.0;
                        dynamics_constraint::LIMITED_CONE
                    }
                    _ => return Err(format!("unknown type {w}")),
                }
            }
            ("--mass", Ok(v)) if v.len() == 1 && v[0] > 0.0 => p.mass = v[0],
            ("--damping", Ok(v)) if v.len() == 1 => p.damping = v[0] as f32,
            ("--gravity", Ok(v)) if v.len() == 1 => p.gravity = [0.0, v[0] as f32, 0.0],
            ("--gravity", Ok(v)) if v.len() == 3 => p.gravity = [v[0] as f32, v[1] as f32, v[2] as f32],
            ("--cone", Ok(v)) if v.len() == 1 => (p.cone_outer, p.cone_outer_end) = (v[0], v[0]),
            ("--cone", Ok(v)) if v.len() == 2 => (p.cone_outer, p.cone_outer_end) = (v[0], v[1]),
            ("--limits", Ok(v)) if v.len() == 4 => p.limit_scales = [v[0], v[1], v[2], v[3]],
            _ => return Err(format!("bad option {a}")),
        }
    }
    Ok(())
}

fn nums_or_word(v: &str) -> Result<Vec<f64>, String> {
    nums(v).map_err(|_| v.to_string())
}

fn retune(m: &mut ModelFile, args: &[String]) -> R<()> {
    let mut d = def(m)?;
    let i = chain_index(m, &d, arg(args, 0)?)?;
    let mut p = d.recover(i).ok_or("chain has no links")?.1;
    apply_params(&args[1..], &mut p)?;
    d.retune(i, &p).map_err(s)?;
    put(m, &d)
}

fn rebuild(m: &mut ModelFile, specs: &[ChainSpec]) -> R<()> {
    let skel = BindSkeleton::from_dat1(&m.dat1).map_err(s)?;
    put(m, &AnimDynamicsDef::build(&skel, specs).map_err(s)?)
}

fn regen(m: &mut ModelFile, args: &[String]) -> R<()> {
    let name = arg(args, 0)?.clone();
    let mut specs = def(m)?.specs(&m.dat1).map_err(s)?;
    let names = joint_names(m)?;
    let (mut joints_arg, mut donor, mut donor_chain, mut opts) = (None, None, None, Vec::new());
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--joints" => joints_arg = Some(it.next().ok_or("--joints needs a value")?),
            "--donor" => donor = Some(it.next().ok_or("--donor needs a value")?),
            "--donor-chain" => donor_chain = Some(it.next().ok_or("--donor-chain needs a value")?),
            _ => opts.push(a.clone()),
        }
    }
    let existing = specs.iter().position(|sp| sp.name == name);
    let (mut joints, mut p) = existing.map(|k| (specs[k].joints.clone(), specs[k].params)).unwrap_or_default();
    if let Some(path) = donor {
        let dm = load(path)?;
        let dd = def(&dm)?;
        let (dj, dp) = dd.recover(chain_index(&dm, &dd, donor_chain.unwrap_or(&name))?).ok_or("donor chain has no links")?;
        let dnames = joint_names(&dm)?;
        joints = dj.iter().map(|&j| joint_index(&names, &dnames[j as usize])).collect::<R<_>>()?;
        p = dp;
    }
    if let Some(list) = joints_arg {
        joints = list.split(',').map(|n| joint_index(&names, n.trim())).collect::<R<_>>()?;
    }
    if joints.is_empty() {
        return Err(format!("new chain {name} needs --joints or --donor"));
    }
    apply_params(&opts, &mut p)?;
    let spec = ChainSpec { name_offset: m.dat1.intern_string(&name), name, joints, params: p };
    match existing {
        Some(k) => specs[k] = spec,
        None => specs.push(spec),
    }
    rebuild(m, &specs)
}

fn drop_chains(m: &mut ModelFile, args: &[String]) -> R<()> {
    let mut specs = def(m)?.specs(&m.dat1).map_err(s)?;
    if let Some(n) = args.iter().find(|n| !specs.iter().any(|sp| &sp.name == *n)) {
        return Err(format!("no chain {n}"));
    }
    specs.retain(|sp| !args.contains(&sp.name));
    if specs.is_empty() {
        return Err("can't drop every chain".into());
    }
    rebuild(m, &specs)
}

fn repose(m: &mut ModelFile, args: &[String]) -> R<()> {
    let specs = def(m)?.specs(&m.dat1).map_err(s)?;
    let names = joint_names(m)?;
    let moves = args
        .iter()
        .map(|a| {
            let (n, v) = a.split_once('=').ok_or_else(|| format!("expected joint=x,y,z, got {a}"))?;
            let v: [f64; 3] = nums(v)?.try_into().map_err(|_| format!("{n} needs x,y,z"))?;
            Ok((joint_index(&names, n)? as usize, v))
        })
        .collect::<R<Vec<_>>>()?;
    if moves.is_empty() {
        return Err("nothing to move".into());
    }
    let parents: Vec<i16> = Joint::parse_all(section(m, TAG_JOINTS)?).map_err(s)?.iter().map(|j| j.parent).collect();
    let mut bind = JointsTransform::parse(section(m, TAG_JOINTS_TRANSFORM)?).map_err(s)?;
    let has_bspheres = m.dat1.get_section_data(TAG_JOINT_BSPHERES).is_some();
    let mut bs = match has_bspheres {
        true => JointBspheres::parse(section(m, TAG_JOINT_BSPHERES)?).map_err(s)?,
        false => JointBspheres::default(),
    };
    repose_joints(&parents, &mut bind, &mut bs, &moves);
    m.dat1.set_section_data(TAG_JOINTS_TRANSFORM, bind.save()).map_err(s)?;
    if has_bspheres {
        m.dat1.set_section_data(TAG_JOINT_BSPHERES, bs.save()).map_err(s)?;
    }
    rebuild(m, &specs)
}

fn set_flag(m: &mut ModelFile) -> R<()> {
    let mut b = section(m, TAG_BUILT)?.to_vec();
    let f = b.get_mut(0x2B).ok_or("Model Built too short")?;
    *f |= 0x80;
    m.dat1.set_section_data(TAG_BUILT, b).map_err(s)
}

const TOL: f32 = 1e-5;

fn near(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= TOL)
}

/// Quaternions match up to sign: the builder's sign isn't always the bind pose's.
fn near_q(a: &[f32; 4], b: &[f32; 4]) -> bool {
    near(a, b) || near(&a.map(|x| -x), b)
}

fn slice<T>(v: &[T], (start, count): (u16, u16)) -> &[T] {
    v.get(start as usize..start as usize + count as usize).unwrap_or(&[])
}

/// Fields where the one-chain rebuild `g` differs from chain `ci` of the shipped section `d`.
fn diff_chain(g: &AnimDynamicsDef, d: &AnimDynamicsDef, ci: usize) -> Vec<String> {
    let (gc, c) = (&g.chains[0], &d.chains[ci]);
    let counts = |c: &DynamicsChain| (c.links.1, c.bends.1, c.points.1, c.attaches.1, c.joint_elems.1);
    if counts(gc) != counts(c) {
        return vec![format!("run counts {:?} vs {:?}", counts(gc), counts(c))];
    }
    let ps = c.points.0;
    let mut bad = Vec::new();
    let mut check = |ok: bool, what: String| {
        if !ok {
            bad.push(what)
        }
    };
    for (k, (gp, sp)) in g.points.iter().zip(slice(&d.points, c.points)).enumerate() {
        check(near(gp, sp), format!("point {k}"));
    }
    for (k, (gl, l)) in g.links.iter().zip(slice(&d.links, c.links)).enumerate() {
        check(
            (gl.point + ps, gl.point_child + ps, gl.joint, gl.joint_child, gl.joint_parent)
                == (l.point, l.point_child, l.joint, l.joint_child, l.joint_parent)
                && near(
                    &[gl.mass_inv, gl.mass_inv_child, gl.rest_length, gl.cone_outer_cos, gl.cone_outer_sin],
                    &[l.mass_inv, l.mass_inv_child, l.rest_length, l.cone_outer_cos, l.cone_outer_sin],
                )
                && near(&gl.cone_limits, &l.cone_limits)
                && near_q(&gl.parent_to_cone_rot, &l.parent_to_cone_rot),
            format!("link {k}"),
        );
    }
    for (k, (gb, b)) in g.bends.iter().zip(slice(&d.bends, c.bends)).enumerate() {
        let parent = if gb.flags & 1 != 0 { gb.parent } else { gb.parent + ps };
        check(
            (parent, gb.base + ps, gb.child + ps, gb.flags) == (b.parent, b.base, b.child, b.flags)
                && near(
                    &[gb.mass_inv, gb.mass_inv_child, gb.bind_distance, gb.cos_angle, gb.stiffness, gb.damping],
                    &[b.mass_inv, b.mass_inv_child, b.bind_distance, b.cos_angle, b.stiffness, b.damping],
                ),
            format!("bend {k}"),
        );
    }
    for (k, (ge, e)) in g.joint_elems.iter().zip(slice(&d.joint_elems, c.joint_elems)).enumerate() {
        check(
            (ge.point_local, ge.point_child_local, ge.joint, ge.joint_child, ge.joint_parent)
                == (e.point_local, e.point_child_local, e.joint, e.joint_child, e.joint_parent)
                && near(&ge.joint_to_child_dir, &e.joint_to_child_dir)
                && near_q(&ge.parent_to_joint_rot, &e.parent_to_joint_rot),
            format!("joint elem {k}"),
        );
    }
    let (gt, t) = (&gc.tethers, &c.tethers);
    check(
        (gc.constraint_type, gc.tether_types, gc.leaf_joint, gc.leaf_joint_parent, gc.damping, gc.name_offset)
            == (c.constraint_type, c.tether_types, c.leaf_joint, c.leaf_joint_parent, c.damping, c.name_offset)
            && (gt[0].point + ps, gt[0].source, gt[0].parent_joint) == (t[0].point, t[0].source, t[0].parent_joint)
            && (gt[1].point, gt[1].source, gt[1].parent_joint) == (t[1].point, t[1].source, t[1].parent_joint)
            && near(&gt[0].parent_to_joint_pos, &t[0].parent_to_joint_pos)
            && near(&gt[1].parent_to_joint_pos, &t[1].parent_to_joint_pos)
            && near_q(&gc.leaf_parent_to_joint_rot, &c.leaf_parent_to_joint_rot)
            && near(&gc.gravity, &c.gravity),
        "chain record".into(),
    );
    // only the first chain's attaches sit where its run points (the builder quirk)
    if ci == 0 {
        for (k, (ga, a)) in g.attaches.iter().zip(slice(&d.attaches, c.attaches)).enumerate() {
            check(
                (ga.attach_point + ps, ga.sim_point + ps) == (a.attach_point, a.sim_point)
                    && near(&[ga.mass_inv_child, ga.geodesic_length], &[a.mass_inv_child, a.geodesic_length]),
                format!("attach {k}"),
            );
        }
    }
    bad
}

fn verify(paths: &[String]) -> R<()> {
    let (mut sections, mut exact, mut chains, mut regenerated, mut skeleton_ok) = (0, 0, 0, 0, 0);
    for path in paths {
        let m = load(path)?;
        let Some(raw) = m.dat1.get_section_data(TAG_ANIM_DYNAMICS_DEF) else { continue };
        sections += 1;
        let d = AnimDynamicsDef::parse(raw).map_err(s)?;
        if d.to_bytes() == raw {
            exact += 1;
        } else {
            println!("{path}: section does not round-trip");
        }
        let bind_raw = section(&m, TAG_JOINTS_TRANSFORM)?;
        let bs_ok = m.dat1.get_section_data(TAG_JOINT_BSPHERES).map_or(true, |b| JointBspheres::parse(b).is_ok_and(|x| x.save() == b));
        if JointsTransform::parse(bind_raw).map_err(s)?.save() == bind_raw && bs_ok {
            skeleton_ok += 1;
        } else {
            println!("{path}: Bind Pose or Joint Bspheres does not round-trip");
        }
        let skel = BindSkeleton::from_dat1(&m.dat1).map_err(s)?;
        for (ci, c) in d.chains.iter().enumerate().filter(|(_, c)| c.tether_types[1] == dynamics_tether::NONE) {
            chains += 1;
            let name = chain_name(&m, c);
            let (joints, params) = d.recover(ci).ok_or("chain has no links")?;
            let spec = ChainSpec { name: name.clone(), name_offset: c.name_offset, joints, params };
            match AnimDynamicsDef::build(&skel, &[spec]) {
                Ok(g) => match diff_chain(&g, &d, ci) {
                    bad if bad.is_empty() => regenerated += 1,
                    bad => println!("{path} {name}: {}", bad.join(", ")),
                },
                Err(e) => println!("{path} {name}: {e}"),
            }
        }
    }
    println!("sections round-tripped byte-exact: {exact}/{sections}");
    println!("bind pose + bspheres round-tripped: {skeleton_ok}/{sections}");
    println!("untethered chains regenerated within {TOL}: {regenerated}/{chains}");
    Ok(())
}
