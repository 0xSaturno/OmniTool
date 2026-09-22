import { useState, useEffect, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import FilePickerInput from "../../components/shared/FilePickerInput";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import styles from "./ModelInspector.module.css";

const MODEL_FILTER = [{ name: "Insomniac Model", extensions: ["model"] }];

interface SectionView {
  tag: number;
  name: string;
  size: number;
  known: boolean;
  name_recovered: boolean;
}
interface BuiltView {
  bsphere_center: [number, number, number];
  bsphere_radius: number;
  aabb_extents: [number, number, number];
  mesh_center: [number, number, number];
  meters_per_unit: number;
  uv0_scale: number;
  uv1_scale: number;
  uv_log_scales: number;
  lod_distances: number[];
  vertex_count: number;
  index_count: number;
  flags: number;
  flag_names: string[];
  content_flags: number;
  ambient_animation: number;
  fade_out_dist: number;
  shadow_fade_dist: number;
  shadow_casting_lod: number;
  av_material_hash: number;
  audio_material_hash: number;
}
interface MaterialSlotView {
  index: number;
  name: string;
  path: string;
  asset_id: string;
  name_hash: number;
  flags: number;
  hash_ok: boolean;
}
interface LodRangeView {
  first_subset: number;
  subset_count: number;
}
interface LookView {
  index: number;
  name: string | null;
  name_hash: number;
  lods: LodRangeView[];
  bspheres: number[];
  mask_ok: boolean;
  rigid_bodies: number;
  cloths: number;
  bvh_usage: number | null;
  bvh_lod: number | null;
}
interface SubsetView {
  index: number;
  material: number;
  material_name: string | null;
  vertex_start: number;
  vertex_count: number;
  index_start: number;
  index_count: number;
  flags: number;
  skinned: boolean;
  lod: number;
  lod_proxy: boolean;
  skin_batches: number;
  anim_vert_batches: number;
  bsphere_radius: number;
  surface_area: number;
  fade_out_dist: number;
  material_lod_dist: number;
  uv_density: [number, number];
}
interface JointView {
  index: number;
  name: string;
  parent: number;
  hash: number;
  subtree_count: number;
  flags: string[];
}
interface LocatorView {
  index: number;
  name: string;
  hash: number;
  joint: number | null;
  joint_name: string | null;
}
interface BsphereView {
  index: number;
  joint: number;
  joint_name: string | null;
  center: [number, number, number];
  radius: number;
}
interface MorphView {
  id: number;
  name: string;
  element_count: number;
  component_bits: number;
  subsets: number[];
  vertex_total: number;
  mirror_of: string | null;
}
interface IkChainView {
  effector_hash: number;
  effector_name: string | null;
  solver_error: number;
  max_iterations: number;
  joints: string[];
}
interface RagdollBodyView {
  joint: string | null;
  parent: string | null;
  ancestors: string[];
}
interface ClothView {
  instances: number;
  collidables: number;
  influence_joints: string[];
}
interface DynamicsChainView {
  name: string;
  constraint: string;
  points: number;
  links: number;
  bends: number;
}
interface HairGroupView {
  name: string;
  name_hash: number;
  strand_count: number;
  first_strand: number;
  point_count: number;
  config: string;
  textures: string[];
  bounds_min: [number, number, number];
  bounds_max: [number, number, number];
}
interface PerfLodView {
  spec: string;
  max_lod: number;
}
interface PhysicsView {
  havok_tagfile: boolean;
  sdk_version: string | null;
  classes: string[];
}
interface ModelInfo {
  path: string;
  sections: SectionView[];
  built: BuiltView | null;
  materials: MaterialSlotView[];
  looks: LookView[];
  subsets: SubsetView[];
  joints: JointView[];
  locators: LocatorView[];
  bspheres: BsphereView[];
  morphs: MorphView[];
  morph_pair_count: number;
  ik_chains: IkChainView[];
  ragdoll_bodies: RagdollBodyView[];
  cloth: ClothView | null;
  dynamics_chains: DynamicsChainView[];
  hair_groups: HairGroupView[];
  perf_max_lod: PerfLodView[];
  render_override_count: number;
  ambient_shadow_prim_count: number;
  physics: PhysicsView | null;
  warnings: string[];
}

type Tab =
  | "overview"
  | "looks"
  | "subsets"
  | "skeleton"
  | "morphs"
  | "hair"
  | "sections";

const TABS: { id: Tab; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "looks", label: "Looks & Materials" },
  { id: "subsets", label: "Subsets" },
  { id: "skeleton", label: "Skeleton" },
  { id: "morphs", label: "Morphs" },
  { id: "hair", label: "Hair" },
  { id: "sections", label: "Sections" },
];

function hex(n: number): string {
  return `0x${(n >>> 0).toString(16).toUpperCase().padStart(8, "0")}`;
}

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function num(n: number): string {
  return n.toLocaleString();
}

function vec3(v: [number, number, number], d = 3): string {
  return v.map((x) => x.toFixed(d)).join(", ");
}

export default function ModelInspector() {
  const location = useLocation();

  const [modelPath, setModelPath] = useState("");
  const [info, setInfo] = useState<ModelInfo | null>(null);
  const [tab, setTab] = useState<Tab>("overview");
  const [filter, setFilter] = useState("");
  const [log, setLog] = useState<LogEntry[]>([]);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    if (location.pathname !== "/tools/model-inspector") return;
    const params = new URLSearchParams(location.search);
    const s = location.state as { filePath?: string } | null;
    const filePath = s?.filePath ?? params.get("filePath") ?? undefined;
    if (filePath) setModelPath(filePath);
  }, [location.pathname, location.state, location.search]);

  const pushLog = useCallback((type: LogEntry["type"], message: string) => {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }, []);

  const load = useCallback(async () => {
    if (!modelPath) {
      pushLog("error", "Select a .model file first.");
      return;
    }
    setRunning(true);
    setLog([]);
    try {
      const result: ModelInfo = await invoke("read_model_info", { modelPath });
      setInfo(result);
      pushLog(
        "success",
        `${result.sections.length} sections · ${num(result.subsets.length)} subsets · ` +
        `${num(result.joints.length)} joints · ${num(result.morphs.length)} morphs`,
      );
      for (const w of result.warnings) pushLog("warning", w);
      if (!result.warnings.length) {
        pushLog("info", "All structural cross-checks passed.");
      }
    } catch (e) {
      setInfo(null);
      pushLog("error", String(e));
    } finally {
      setRunning(false);
    }
  }, [modelPath, pushLog]);

  const q = filter.trim().toLowerCase();
  const match = useCallback((...fields: (string | null | undefined)[]) =>
    !q || fields.some((f) => f && f.toLowerCase().includes(q)), [q]);

  const morphs = useMemo(
    () => (info ? info.morphs.filter((m) => match(m.name, m.mirror_of)) : []),
    [info, match],
  );
  const joints = useMemo(
    () => (info ? info.joints.filter((j) => match(j.name)) : []),
    [info, match],
  );
  const locators = useMemo(
    () => (info ? info.locators.filter((l) => match(l.name, l.joint_name)) : []),
    [info, match],
  );
  const subsets = useMemo(
    () =>
      info
        ? info.subsets.filter((s) => match(s.material_name, String(s.index)))
        : [],
    [info, match],
  );

  const showFilter = tab === "morphs" || tab === "skeleton" || tab === "subsets";

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>Model Inspector</h2>
        <span className={styles.subtitle}>
          Read-only view of every reversed <code>.model</code> section (looks, materials, skeleton, ragdoll, cloth, IK, jiggle chains, morph targets and hair).
        </span>
      </div>

      <div className={styles.panel}>
        <FilePickerInput
          label="Model file"
          value={modelPath}
          onChange={setModelPath}
          mode="open"
          filters={MODEL_FILTER}
          placeholder="Select a .model file"
        />
        <div className={styles.actions}>
          <button className={styles.primary} onClick={load} disabled={running || !modelPath}>
            {running ? "Reading…" : "Inspect"}
          </button>
          {showFilter && info && (
            <input
              className={styles.search}
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="Filter by name…"
            />
          )}
        </div>
      </div>

      {info && (
        <div className={styles.tabs}>
          {TABS.map((t) => (
            <button
              key={t.id}
              className={tab === t.id ? styles.tabActive : styles.tab}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>
      )}

      {info && (
        <div className={styles.content}>
          {tab === "overview" && (
            <div className={styles.cards}>
              <div className={styles.card}>
                <h3>Geometry</h3>
                {info.built ? (
                  <dl className={styles.dl}>
                    <dt>Vertices</dt>
                    <dd>{num(info.built.vertex_count)}</dd>
                    <dt>Indices</dt>
                    <dd>{num(info.built.index_count)}</dd>
                    <dt>Subsets</dt>
                    <dd>{num(info.subsets.length)}</dd>
                    <dt>Meters per unit</dt>
                    <dd>1 / {(1 / info.built.meters_per_unit).toFixed(1)}</dd>
                    <dt>UV0 / UV1 scale</dt>
                    <dd>
                      1/{Math.round(1 / info.built.uv0_scale)} · 1/
                      {Math.round(1 / info.built.uv1_scale)}{" "}
                      <span className={styles.dim}>
                        (field {hex(info.built.uv_log_scales)})
                      </span>
                    </dd>
                    <dt>Bounding sphere</dt>
                    <dd>
                      r {info.built.bsphere_radius.toFixed(3)}{" "}
                      <span className={styles.dim}>@ {vec3(info.built.bsphere_center)}</span>
                    </dd>
                    <dt>Box half-extents</dt>
                    <dd>{vec3(info.built.aabb_extents)}</dd>
                    <dt>LOD distances</dt>
                    <dd>{info.built.lod_distances.join(", ")}</dd>
                    <dt>Shadow LOD</dt>
                    <dd>
                      {info.built.shadow_casting_lod < 0 ? "—" : info.built.shadow_casting_lod}
                      {info.built.shadow_fade_dist > 0 && (
                        <span className={styles.dim}> · fades at {info.built.shadow_fade_dist}</span>
                      )}
                    </dd>
                    {info.perf_max_lod.length > 0 && (
                      <>
                        <dt>Perf LOD clamp</dt>
                        <dd>
                          {info.perf_max_lod.map((p) => `${p.spec}: ${p.max_lod}`).join(" · ")}
                        </dd>
                      </>
                    )}
                  </dl>
                ) : (
                  <p className={styles.dim}>No Model Built section.</p>
                )}
              </div>

              <div className={styles.card}>
                <h3>Features</h3>
                {info.built && (
                  <>
                    <p className={styles.mono}>{hex(info.built.flags)}</p>
                    {info.built.flag_names.length ? (
                      <ul className={styles.list}>
                        {info.built.flag_names.map((f) => (
                          <li key={f}>{f}</li>
                        ))}
                      </ul>
                    ) : (
                      <p className={styles.dim}>No flags set.</p>
                    )}
                  </>
                )}
                <dl className={styles.dl}>
                  <dt>Looks</dt>
                  <dd>{info.looks.length}</dd>
                  <dt>Materials</dt>
                  <dd>{info.materials.length}</dd>
                  <dt>Joints / locators</dt>
                  <dd>
                    {info.joints.length} / {info.locators.length}
                  </dd>
                  <dt>Morph targets</dt>
                  <dd>
                    {info.morphs.length}
                    {info.morph_pair_count > 0 && (
                      <span className={styles.dim}> ({info.morph_pair_count} L/R pairs)</span>
                    )}
                  </dd>
                  <dt>Hair groups</dt>
                  <dd>{info.hair_groups.length}</dd>
                  <dt>Ragdoll bodies</dt>
                  <dd>{info.ragdoll_bodies.length}</dd>
                  <dt>Jiggle chains</dt>
                  <dd>{info.dynamics_chains.length}</dd>
                  <dt>Render overrides</dt>
                  <dd>{info.render_override_count}</dd>
                  <dt>Shadow capsules</dt>
                  <dd>{info.ambient_shadow_prim_count}</dd>
                </dl>
              </div>

              <div className={styles.card}>
                <h3>Physics</h3>
                {info.physics?.havok_tagfile ? (
                  <dl className={styles.dl}>
                    <dt>Format</dt>
                    <dd>Havok tagfile</dd>
                    <dt>SDK</dt>
                    <dd>{info.physics.sdk_version ?? "—"}</dd>
                    <dt>Classes</dt>
                    <dd className={styles.mono}>
                      {info.physics.classes.join(", ") || "—"}
                    </dd>
                  </dl>
                ) : (
                  <p className={styles.dim}>No physics data.</p>
                )}
                <h3 style={{ marginTop: "1rem" }}>IK chains</h3>
                {info.ik_chains.length ? (
                  <ul className={styles.list}>
                    {info.ik_chains.map((c) => (
                      <li key={c.effector_hash}>
                        <span className={styles.mono}>
                          {c.effector_name ?? hex(c.effector_hash)}
                        </span>
                        <br />
                        <span className={styles.dim}>
                          {c.joints.join(" › ")} · {c.max_iterations} iterations
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className={styles.dim}>None.</p>
                )}
                {info.cloth && (
                  <>
                    <h3 style={{ marginTop: "1rem" }}>Cloth</h3>
                    <p className={styles.dim}>
                      {info.cloth.instances} instance(s), {info.cloth.collidables} collidables,{" "}
                      {info.cloth.influence_joints.length} influence joints
                    </p>
                  </>
                )}
              </div>
            </div>
          )}

          {tab === "looks" && (
            <div className={styles.split}>
              <div>
                <h3 className={styles.sectionTitle}>Looks</h3>
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>Name</th>
                      <th>LOD 0 subsets</th>
                      <th>Bspheres</th>
                      <th>Bodies / cloth</th>
                      <th>BVH</th>
                    </tr>
                  </thead>
                  <tbody>
                    {info.looks.map((l) => (
                      <tr key={l.index}>
                        <td>{l.index}</td>
                        <td>
                          {l.name ?? <span className={styles.mono}>{hex(l.name_hash)}</span>}
                          {!l.mask_ok && <span className={styles.warn}> mask mismatch</span>}
                        </td>
                        <td>
                          {l.lods[0]
                            ? `${l.lods[0].first_subset} … ${l.lods[0].first_subset + l.lods[0].subset_count
                            }`
                            : "—"}
                        </td>
                        <td>{l.bspheres.length}</td>
                        <td>
                          {l.rigid_bodies} / {l.cloths}
                        </td>
                        <td className={styles.dim}>
                          {l.bvh_lod === null ? "—" : `LOD ${l.bvh_lod} · ${hex(l.bvh_usage ?? 0)}`}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <div>
                <h3 className={styles.sectionTitle}>Materials</h3>
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>Slot</th>
                      <th>Asset id</th>
                      <th>Path</th>
                    </tr>
                  </thead>
                  <tbody>
                    {info.materials.map((m) => (
                      <tr key={m.index}>
                        <td>{m.index}</td>
                        <td>
                          {m.name}
                          {!m.hash_ok && <span className={styles.warn}> hash?</span>}
                        </td>
                        <td className={styles.mono}>
                          {m.asset_id === "0000000000000000" ? "—" : m.asset_id}
                        </td>
                        <td className={styles.pathCell} title={m.path}>
                          {m.path || <span className={styles.dim}>none</span>}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {tab === "subsets" && (
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>#</th>
                  <th>Material</th>
                  <th>Verts</th>
                  <th>Indices</th>
                  <th>Flags</th>
                  <th>LOD</th>
                  <th>Skin batches</th>
                  <th>Radius / area</th>
                  <th>UV density (u, v)</th>
                </tr>
              </thead>
              <tbody>
                {subsets.map((sv) => (
                  <tr key={sv.index}>
                    <td>{sv.index}</td>
                    <td>
                      {sv.material_name ?? sv.material}
                    </td>
                    <td>
                      {num(sv.vertex_count)}{" "}
                      <span className={styles.dim}>@{num(sv.vertex_start)}</span>
                    </td>
                    <td>{num(sv.index_count)}</td>
                    <td className={styles.mono}>
                      0x{sv.flags.toString(16).toUpperCase().padStart(4, "0")}
                    </td>
                    <td>
                      {sv.lod}
                      {sv.lod_proxy && <span className={styles.dim}> shared</span>}
                    </td>
                    <td>
                      {sv.skinned ? sv.skin_batches : "—"}
                      {sv.anim_vert_batches > 0 && (
                        <span className={styles.dim}> ({sv.anim_vert_batches} morph)</span>
                      )}
                    </td>
                    <td className={styles.mono}>
                      {sv.bsphere_radius.toFixed(2)} m / {sv.surface_area.toFixed(2)} m²
                    </td>
                    <td className={styles.mono}>
                      {sv.uv_density[0].toFixed(2)}, {sv.uv_density[1].toFixed(2)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {tab === "skeleton" && (
            <div className={styles.split}>
              <div>
                <h3 className={styles.sectionTitle}>Joints ({joints.length})</h3>
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th>#</th>
                      <th>Name</th>
                      <th>Parent</th>
                      <th>Flags</th>
                    </tr>
                  </thead>
                  <tbody>
                    {joints.map((j) => (
                      <tr key={j.index}>
                        <td>{j.index}</td>
                        <td>{j.name}</td>
                        <td className={styles.dim}>
                          {j.parent < 0
                            ? "root"
                            : info.joints[j.parent]?.name ?? j.parent}
                        </td>
                        <td className={styles.dim}>{j.flags.join(", ")}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <div>
                <h3 className={styles.sectionTitle}>Locators ({locators.length})</h3>
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Attached to</th>
                    </tr>
                  </thead>
                  <tbody>
                    {locators.map((l) => (
                      <tr key={l.index}>
                        <td>{l.name}</td>
                        <td className={styles.dim}>{l.joint_name ?? "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>

                {info.ragdoll_bodies.length > 0 && (
                  <>
                    <h3 className={styles.sectionTitle} style={{ marginTop: "1rem" }}>
                      Ragdoll bodies ({info.ragdoll_bodies.length})
                    </h3>
                    <ul className={styles.list}>
                      {info.ragdoll_bodies.map((b, i) => (
                        <li key={`${b.joint}-${i}`}>
                          <strong>{b.joint ?? "(no joint)"}</strong>
                          <br />
                          <span className={styles.dim}>
                            {b.ancestors.length ? b.ancestors.join(" › ") : "root"}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </>
                )}

                {info.dynamics_chains.length > 0 && (
                  <>
                    <h3 className={styles.sectionTitle} style={{ marginTop: "1rem" }}>
                      Jiggle chains ({info.dynamics_chains.length})
                    </h3>
                    <ul className={styles.list}>
                      {info.dynamics_chains.map((c, i) => (
                        <li key={`${c.name}-${i}`}>
                          <strong>{c.name || `chain ${i}`}</strong>{" "}
                          <span className={styles.dim}>
                            {c.constraint} · {c.points} points · {c.links} links · {c.bends} bends
                          </span>
                        </li>
                      ))}
                    </ul>
                  </>
                )}

                {info.cloth && info.cloth.influence_joints.length > 0 && (
                  <>
                    <h3 className={styles.sectionTitle} style={{ marginTop: "1rem" }}>
                      Cloth influence joints ({info.cloth.influence_joints.length})
                    </h3>
                    <p className={styles.dim}>{info.cloth.influence_joints.join(", ")}</p>
                  </>
                )}
              </div>
            </div>
          )}

          {tab === "morphs" && (
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Mirror</th>
                  <th>Vertices</th>
                  <th>Subsets</th>
                  <th>Precision</th>
                </tr>
              </thead>
              <tbody>
                {morphs.map((m) => (
                  <tr key={m.id}>
                    <td>{m.name}</td>
                    <td className={styles.dim}>{m.mirror_of ?? "—"}</td>
                    <td>{num(m.vertex_total)}</td>
                    <td className={styles.dim}>{m.subsets.join(", ")}</td>
                    <td className={styles.dim}>
                      {m.component_bits} bit × {m.element_count}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {tab === "hair" && (
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>Group</th>
                  <th>Strands</th>
                  <th>Control points</th>
                  <th>Config</th>
                  <th>Bounds min</th>
                  <th>Bounds max</th>
                </tr>
              </thead>
              <tbody>
                {info.hair_groups.map((g, i) => (
                  <tr key={`${g.name_hash}-${i}`}>
                    <td>{g.name}</td>
                    <td>{num(g.strand_count)}</td>
                    <td>{num(g.point_count)}</td>
                    <td className={styles.pathCell} title={[g.config, ...g.textures].join("\n")}>
                      {g.config || <span className={styles.dim}>none</span>}
                    </td>
                    <td className={styles.mono}>{vec3(g.bounds_min)}</td>
                    <td className={styles.mono}>{vec3(g.bounds_max)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {tab === "sections" && (
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>Tag</th>
                  <th>Name</th>
                  <th>Size</th>
                </tr>
              </thead>
              <tbody>
                {info.sections.map((sec) => (
                  <tr key={sec.tag}>
                    <td className={styles.mono}>{hex(sec.tag)}</td>
                    <td className={sec.known ? undefined : styles.dim}>
                      {sec.name}
                      {sec.known && !sec.name_recovered && (
                        <span className={styles.dim}> (inferred)</span>
                      )}
                    </td>
                    <td>{bytes(sec.size)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}

      <StatusLog entries={log} />
    </div>
  );
}
