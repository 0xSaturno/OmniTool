//! Dev harness: pull the compiled GPU programs out of a `.materialgraph`.
//!
//!   cargo run --release --bin shader_dump -- <game dir> <mode> [arg] [arg2]
//!
//! A built `.materialgraph` carries every compiled program for the graph in one
//! blob section, `0xBBFC8900` "Shader GPU Program Data", indexed by a fixed
//! 18-slot table `0x8C049CCA` of 16-byte records
//! `{u32 flags, u32 hash, u32 offset, u32 size}`. A zeroed record means the
//! graph has no program for that slot; the slot index is the render pass /
//! stage, so the same slot number means the same job across graphs.
//!
//! Each program blob is itself a 12-byte `IGSH` wrapper
//! `{"IGSH", u32 sm5_len, u32 sm6_len}` followed by **two** DXBC containers: the
//! shader-model 5 build (`RDEF`/`ISGN`/`OSGN`/`SHEX`/`STAT`) and the shader-model
//! 6 build (`SFI0`/`ISG1`/`OSG1`/`PSV0`/`STAT`/`HASH`/`DXIL`). The PC build is the
//! Nixxes port, so there is no PS5 GCN anywhere in here. The SM5 half is the one
//! worth reading: `fxc /dumpbin` disassembles it.
//!
//! Modes:
//!   index <graph path>            the program table, one line per program
//!   extract <graph path> <dir>    each program's two containers to
//!                                 <dir>/slotNN_<hash>.{dxbc,dxil}
//!   probe <substr>                the table for every matching `.materialgraph`
//!   slots <substr>                which slots each matching graph fills
//!   cbmap <graph path>            `cb2[r].c` -> template constant hash, the
//!                                 key for reading a disassembly
//!   annotate <graph path> <asm>   rewrite one `fxc` listing with every
//!                                 `cb2[r].c` and `tN` replaced by its hash
//!
//! `index` names each program's stage and render-target count, read out of the
//! SM5 container's `SHEX` version token and `OSGN` chunk. The deferred G-buffer
//! pass is the pixel shader with four render targets.
//!
//! To disassemble what `extract` wrote (PowerShell, not Git Bash — `/dumpbin`
//! gets path-mangled there):
//!
//! ```text
//! & "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe" `
//!     /nologo /dumpbin slot04_258E751E.dxbc /Fc slot04.asm
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::material::MaterialFile;
use omnitool_lib::core::{crc64, level};

pub const TAG_SHADER_GPU_PROGRAM_DATA: u32 = 0xBBFC_8900;
pub const TAG_SHADER_PROGRAM_INDEX: u32 = 0x8C04_9CCA;
const INDEX_ENTRY_SIZE: usize = 16;

#[derive(Debug, Clone, Copy)]
struct ProgramEntry {
    slot: usize,
    flags: u32,
    hash: u32,
    offset: u32,
    size: u32,
}

fn rd_u32(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .unwrap_or(0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = args.next().unwrap_or_else(|| "index".into());
    let arg = args
        .next()
        .unwrap_or_else(|| "materialgraph/global/gbl_rgb_blend/gbl_rgb_blend.materialgraph".into());
    let arg2 = args.next().unwrap_or_default();

    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let idx = level::get_index(&game, Some(&hashes)).expect("index");
    idx.require_names().expect("hashes file");

    match mode.as_str() {
        "index" => index_mode(&idx, &arg),
        "extract" => extract_mode(&idx, &arg, &arg2),
        "probe" => probe_mode(&idx, &arg),
        "slots" => slots_mode(&idx, &arg),
        "cbmap" => cbmap_mode(&idx, &arg),
        "annotate" => annotate_mode(&idx, &arg, &arg2),
        _ => eprintln!("unknown mode {mode}"),
    }
}

fn load(idx: &level::LevelIndex, path: &str) -> Option<Dat1> {
    let p = crc64::normalize_path(path);
    let id = crc64::hash_raw(p.as_bytes());
    let raw = idx.extract(id).ok()?;
    MaterialFile::parse(&raw).ok().map(|m| m.dat1)
}

fn matching(idx: &level::LevelIndex, pat: &str) -> Vec<String> {
    let pat = pat.to_ascii_lowercase();
    let mut v: Vec<String> = idx
        .names
        .iter()
        .filter(|(id, n)| {
            let n = n.to_ascii_lowercase();
            n.contains(&pat) && n.ends_with(".materialgraph") && idx.asset(**id).is_some()
        })
        .map(|(_, n)| n.to_ascii_lowercase())
        .collect();
    v.sort();
    v
}

/// The program table. Zeroed slots are dropped; the slot index is kept because
/// it, not the position in the returned list, is what identifies the pass.
fn programs(d: &Dat1) -> Vec<ProgramEntry> {
    let Some(sec) = d.get_section_data(TAG_SHADER_PROGRAM_INDEX) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for slot in 0..sec.len() / INDEX_ENTRY_SIZE {
        let base = slot * INDEX_ENTRY_SIZE;
        let e = ProgramEntry {
            slot,
            flags: rd_u32(sec, base),
            hash: rd_u32(sec, base + 4),
            offset: rd_u32(sec, base + 8),
            size: rd_u32(sec, base + 12),
        };
        if e.flags == 0 && e.hash == 0 && e.size == 0 {
            continue;
        }
        out.push(e);
    }
    out
}

fn blob<'a>(d: &'a Dat1, e: &ProgramEntry) -> Option<&'a [u8]> {
    let sec = d.get_section_data(TAG_SHADER_GPU_PROGRAM_DATA)?;
    sec.get(e.offset as usize..(e.offset as usize).saturating_add(e.size as usize))
}

/// What container a blob is, judged only by its leading bytes.
fn magic(b: &[u8]) -> String {
    if b.len() < 8 {
        return "(too short)".into();
    }
    let tag: String = b[..4]
        .iter()
        .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
        .collect();
    let known = match &b[..4] {
        b"IGSH" => "Insomniac program wrapper",
        b"DXBC" => "DXBC container",
        b"DXIL" => "DXIL wrapper",
        b"BC\xC0\xDE" => "bare LLVM bitcode",
        b"OrbS" => "OrbShaderBinary (PS4/PS5)",
        _ => "unrecognised",
    };
    format!("{tag:?} = {known}")
}

/// The two DXBC containers inside one `IGSH` program: the shader-model 5 build
/// and the shader-model 6 build.
fn halves<'a>(b: &'a [u8]) -> Option<(&'a [u8], &'a [u8])> {
    if b.len() < 12 || &b[..4] != b"IGSH" {
        return None;
    }
    let sm5 = rd_u32(b, 4) as usize;
    let sm6 = rd_u32(b, 8) as usize;
    Some((b.get(12..12 + sm5)?, b.get(12 + sm5..12 + sm5 + sm6)?))
}

/// The chunk table of a DXBC container: `{four-cc: payload}`, payload starting
/// after each chunk's own 8-byte `{tag, size}` header.
fn dxbc_chunks(p: &[u8]) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    if p.len() < 32 || &p[..4] != b"DXBC" {
        return out;
    }
    let n = rd_u32(p, 28) as usize;
    for i in 0..n {
        let off = rd_u32(p, 32 + i * 4) as usize;
        if off + 8 > p.len() {
            break;
        }
        let tag = String::from_utf8_lossy(&p[off..off + 4]).into_owned();
        out.push((tag, off + 8, rd_u32(p, off + 4) as usize));
    }
    out
}

/// Stage and render-target count of an SM5 container, from `SHEX` and `OSGN`.
/// The version token's high half-word is the program type: 0 pixel, 1 vertex,
/// 2 geometry, 3 hull, 4 domain, 5 compute.
fn sm5_stage(p: &[u8]) -> String {
    let mut stage = "?".to_string();
    let mut model = String::new();
    let mut targets = None;
    for (tag, base, _) in dxbc_chunks(p) {
        match tag.as_str() {
            "SHEX" | "SHDR" => {
                let v = rd_u32(p, base);
                stage = match v >> 16 {
                    0 => "pixel",
                    1 => "vertex",
                    2 => "geometry",
                    3 => "hull",
                    4 => "domain",
                    5 => "compute",
                    _ => "?",
                }
                .to_string();
                model = format!(" sm{}.{}", (v >> 4) & 0xF, v & 0xF);
            }
            "OSGN" | "OSG5" | "OSG1" => targets = Some(rd_u32(p, base) as usize),
            _ => {}
        }
    }
    match targets {
        Some(n) => format!("{stage}{model}, {n} output(s)"),
        None => format!("{stage}{model}"),
    }
}

fn index_mode(idx: &level::LevelIndex, path: &str) {
    let Some(d) = load(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    let blob_len = d.get_section_data(TAG_SHADER_GPU_PROGRAM_DATA).map_or(0, |s| s.len());
    println!("== {path}");
    println!("   program data {blob_len} bytes");
    let table = d.get_section_data(TAG_SHADER_PROGRAM_INDEX).map_or(0, |s| s.len());
    println!("   index table {table} bytes = {} slots", table / INDEX_ENTRY_SIZE);
    for e in programs(&d) {
        let Some(b) = blob(&d, &e) else {
            println!("   slot {:2}  <offset/size out of range>", e.slot);
            continue;
        };
        let detail = match halves(b) {
            Some((sm5, sm6)) => format!(
                "sm5 {:>7} B {}  |  sm6 {:>7} B [{}]",
                sm5.len(),
                sm5_stage(sm5),
                sm6.len(),
                dxbc_chunks(sm6).iter().map(|c| c.0.clone()).collect::<Vec<_>>().join(" ")
            ),
            None => magic(b),
        };
        println!(
            "   slot {:2}  flags {:#06X}  hash {:08X}  off {:#09X}  size {:>8}  {detail}",
            e.slot, e.flags, e.hash, e.offset, e.size,
        );
    }
}

fn extract_mode(idx: &level::LevelIndex, path: &str, dir: &str) {
    if dir.is_empty() {
        eprintln!("extract needs an output directory");
        return;
    }
    let Some(d) = load(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    let dir = Path::new(dir);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        return;
    }
    for e in programs(&d) {
        let Some(b) = blob(&d, &e) else {
            eprintln!("slot {} out of range", e.slot);
            continue;
        };
        let stem = format!("slot{:02}_{:08X}", e.slot, e.hash);
        match halves(b) {
            Some((sm5, sm6)) => {
                for (ext, part) in [("dxbc", sm5), ("dxil", sm6)] {
                    let out = dir.join(format!("{stem}.{ext}"));
                    if let Err(err) = std::fs::write(&out, part) {
                        eprintln!("{}: {err}", out.display());
                    }
                }
                println!("{stem}.{{dxbc,dxil}}  {} / {} bytes  {}", sm5.len(), sm6.len(), sm5_stage(sm5));
            }
            None => {
                // Not an IGSH wrapper: keep the bytes verbatim so the caller can
                // look at whatever this platform emitted.
                let out = dir.join(format!("{stem}.bin"));
                match std::fs::write(&out, b) {
                    Ok(()) => println!("{} {} bytes  {}", out.display(), b.len(), magic(b)),
                    Err(err) => eprintln!("{}: {err}", out.display()),
                }
            }
        }
    }
}

/// `cb2[register].component` -> the template constant that lives there.
///
/// The material constant buffer the pixel shader reads (`MultiCustomMaterialCB`
/// / `g_CustomMaterial_Buffer`, register `b2`) is laid out by the template's own
/// constant table, whose `buffer_offset` is already `register * 16 + component *
/// 4`. So a `cb2[3].zw` in a disassembly is exactly the constant at offset 56,
/// and the map below is all it takes to turn a listing into names.
///
/// Texture units follow the same trick: the template's `slot_index` is
/// `HWRegister - TEX_UNIT_BASE` with `TEX_UNIT_BASE == 5`, so the `t5` of a
/// disassembly is sampler slot 0.
const TEX_UNIT_BASE: u16 = 5;

fn cb_labels(t: &omnitool_lib::core::material_graph::MaterialTemplate) -> BTreeMap<String, String> {
    let comp = ['x', 'y', 'z', 'w'];
    let mut out = BTreeMap::new();
    for c in &t.constants {
        let reg = c.buffer_offset / 16;
        let first = (c.buffer_offset % 16) / 4;
        let n = (c.buffer_size as usize / 4).clamp(1, 4);
        let swz: String = (0..n).map(|k| comp[(first as usize + k).min(3)]).collect();
        let vals: Vec<String> = c.default_values.iter().map(|v| format!("{v}")).collect();
        out.insert(
            format!("cb2[{reg}].{swz}"),
            format!("{:08X} [{}]", c.name_hash, vals.join(", ")),
        );
    }
    out
}

fn tex_labels(t: &omnitool_lib::core::material_graph::MaterialTemplate) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for s in &t.samplers {
        out.insert(
            format!("t{}", s.slot_index + TEX_UNIT_BASE),
            format!("slot{} {:08X} {}", s.slot_index, s.name_hash, s.default_path),
        );
    }
    out
}

fn template_of(idx: &level::LevelIndex, path: &str) -> Option<omnitool_lib::core::material_graph::MaterialTemplate> {
    let d = load(idx, path)?;
    omnitool_lib::core::material_graph::MaterialTemplate::from_dat1(&d).ok()
}

fn cbmap_mode(idx: &level::LevelIndex, path: &str) {
    let Some(t) = template_of(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    println!("== {path}");
    println!("   -- texture units (as a disassembly names them) --");
    for (k, v) in tex_labels(&t) {
        println!("   {k:<4} {v}");
    }
    println!("   -- material constant buffer b2 --");
    for (k, v) in cb_labels(&t) {
        println!("   {k:<12} {v}");
    }
}

/// Substitute every `cb2[r].swz` and `tN` in an `fxc` listing for the template
/// constant hash / sampler slot it refers to, so the arithmetic can be read
/// without counting registers by hand. Longest keys first, so `cb2[10]` is not
/// eaten by `cb2[1]`.
fn annotate_mode(idx: &level::LevelIndex, path: &str, asm: &str) {
    if asm.is_empty() {
        eprintln!("annotate needs the path of an fxc listing");
        return;
    }
    let Some(t) = template_of(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    let text = match std::fs::read_to_string(asm) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{asm}: {e}");
            return;
        }
    };
    // Which constant owns each (register, component) cell of b2.
    let mut owner: BTreeMap<(u16, u16), u32> = BTreeMap::new();
    for c in &t.constants {
        for k in 0..(c.buffer_size / 4).max(1) {
            let off = c.buffer_offset + k * 4;
            owner.insert((off / 16, (off % 16) / 4), c.name_hash);
        }
    }

    let mut out = String::with_capacity(text.len() * 2);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if text[i..].starts_with("cb2[") {
            if let Some(rest) = read_cb_ref(&text[i..]) {
                let (reg, swz, len) = rest;
                // One label per distinct constant touched, in swizzle order.
                let mut parts: Vec<String> = Vec::new();
                for ch in swz.chars() {
                    let comp = match ch {
                        'x' => 0,
                        'y' => 1,
                        'z' => 2,
                        'w' => 3,
                        _ => continue,
                    };
                    let label = match owner.get(&(reg, comp)) {
                        Some(h) => format!("{h:08X}.{ch}"),
                        None => format!("?.{ch}"),
                    };
                    if parts.last() != Some(&label) {
                        parts.push(label);
                    }
                }
                out.push('<');
                out.push_str(&parts.join("|"));
                out.push('>');
                i += len;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    // Texture units are unambiguous, so a plain substitution is enough; do the
    // two-digit units first so `t1` cannot eat `t11`.
    let mut tex: Vec<(String, String)> = tex_labels(&t)
        .iter()
        .map(|(k, v)| {
            (k.clone(), format!("<{}>", v.split_whitespace().nth(1).unwrap_or("")))
        })
        .collect();
    tex.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    for (unit, label) in tex {
        for sep in ['.', ','] {
            out = out.replace(&format!("{unit}{sep}"), &format!("{label}{sep}"));
        }
    }
    print!("{out}");
}

/// Parse a `cb2[<reg>].<swizzle>` reference at the start of `s`, returning the
/// register, the swizzle letters and how many bytes the whole reference spans.
/// Anything indexed by a register (`cb2[r1.x + 0]`) is left alone.
fn read_cb_ref(s: &str) -> Option<(u16, String, usize)> {
    let rest = s.strip_prefix("cb2[")?;
    let close = rest.find(']')?;
    let reg: u16 = rest[..close].parse().ok()?;
    let after = &rest[close + 1..];
    let swz: String = after.strip_prefix('.')?.chars().take_while(|c| "xyzw".contains(*c)).collect();
    if swz.is_empty() {
        return None;
    }
    Some((reg, swz.clone(), 4 + close + 2 + swz.len()))
}

fn probe_mode(idx: &level::LevelIndex, pat: &str) {
    for p in matching(idx, pat) {
        index_mode(idx, &p);
    }
}

fn slots_mode(idx: &level::LevelIndex, pat: &str) {
    let mut by_slot: BTreeMap<usize, Vec<(String, u32, u32)>> = BTreeMap::new();
    for p in matching(idx, pat) {
        let Some(d) = load(idx, &p) else { continue };
        for e in programs(&d) {
            by_slot
                .entry(e.slot)
                .or_default()
                .push((p.rsplit('/').next().unwrap_or(&p).to_string(), e.flags, e.size));
        }
    }
    for (slot, v) in by_slot {
        let flags: std::collections::BTreeSet<u32> = v.iter().map(|x| x.1).collect();
        println!(
            "slot {slot:2}  {:>3} graphs  flags {:?}",
            v.len(),
            flags.iter().map(|f| format!("{f:#06X}")).collect::<Vec<_>>()
        );
        for (name, _, size) in v.iter().take(6) {
            println!("        {size:>8}  {name}");
        }
    }
}
