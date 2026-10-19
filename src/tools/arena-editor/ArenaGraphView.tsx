import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type RefObject,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  MiniMap,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import ELK from "elkjs/lib/elk.bundled.js";
import "@xyflow/react/dist/style.css";
import styles from "./ArenaGraphView.module.css";

interface GraphParam {
  pin: string;
  var: number;
  name: string | null;
  value: string;
  write: boolean;
}

interface GraphNode {
  index: number;
  kind: string;
  label: string | null;
  relay: "emit" | "listen" | "both" | "idle" | null;
  global: boolean;
  prius_id: number | null;
  signal: number | null;
  params: GraphParam[];
  section: number;
  node_id: string;
  template_id: string;
}

interface GraphLink {
  from: number;
  from_pin: string;
  to: number;
  to_pin: string;
}

interface GraphSection {
  key: string;
  label: string;
  kind: "wave" | "chunk" | "loose";
  nodes: number[];
}

interface GraphSignal {
  name: string;
  global: boolean;
  emitters: number[];
  listeners: number[];
}

interface ArenaGraph {
  nodes: GraphNode[];
  links: GraphLink[];
  sections: GraphSection[];
  signals: GraphSignal[];
  pins_named: number;
  pins_total: number;
}

interface PriusRef {
  id: number;
  json: string;
}

// ---------------------------------------------------------------------------
// look

type Category = "event" | "signal" | "flow" | "logic" | "spawn" | "ui" | "world";

const CATEGORY_COLOR: Record<Category, string> = {
  event: "#3fb950",
  signal: "#c58af9",
  flow: "#58a6ff",
  logic: "#79c0ff",
  spawn: "#f0883e",
  ui: "#39c5cf",
  world: "#8b949e",
};

const CATEGORY_LABEL: Record<Category, string> = {
  event: "Event",
  signal: "Signal",
  flow: "Flow",
  logic: "Logic / data",
  spawn: "Spawning",
  ui: "HUD / dialog",
  world: "Actors / world",
};

function categoryOf(kind: string): Category {
  if (/^On[A-Z]/.test(kind) || kind === "ScriptStarted" || kind.endsWith("Listener")) return "event";
  if (/Signal/.test(kind)) return "signal";
  if (/^(Delay|Gate|PassThru|Once|Timer|Periodic)$/.test(kind)) return "flow";
  if (/^(Compare|Counter|Set|Get|Add|Clamp|IsVarConnected|And|Or|Not|Random)/.test(kind)) return "logic";
  if (/Spawn|Factory|ActorGroup|ActorPick|ActorCount|Kill/.test(kind)) return "spawn";
  if (/^(UI|HUD|PlayDialog|OverlayFade|PrototypeMessage|DebugMessage|DebugPrint)/.test(kind)) return "ui";
  return "world";
}

// Fixed row heights, so the layout can size nodes before they render.
const NODE_W = 236;
const GHOST_W = 196;
const HEADER_H = 24;
const LABEL_H = 18;
const PROP_H = 16;
const ROW_H = 18;
const PARAM_H = 16;
const PAD_H = 8;
const GHOST_H = 44;
const MAX_PARAMS = 6;
const MAX_GHOSTS = 60;
const GRID_MINOR = 24;
const PALETTE_W = 420;
const PALETTE_H = 380;
const MAX_HITS = 80;

/** Relays carry their named pairing on a pseudo pin. */
const SIGNAL_PIN = "~signal";

interface ScriptNodeData extends Record<string, unknown> {
  node: GraphNode;
  inputs: string[];
  outputs: string[];
  props: string[];
  color: string;
}

interface GhostNodeData extends Record<string, unknown> {
  node: GraphNode;
  section: string;
  color: string;
}

type ScriptFlowNode = Node<ScriptNodeData, "script">;
type GhostFlowNode = Node<GhostNodeData, "ghost">;
type FlowNode = ScriptFlowNode | GhostFlowNode;

function paramRows(n: GraphNode) {
  return Math.min(n.params.length, MAX_PARAMS) + (n.params.length > MAX_PARAMS ? 1 : 0);
}

function nodeHeight(d: ScriptNodeData) {
  const rows = Math.max(d.inputs.length, d.outputs.length);
  return (
    HEADER_H +
    (d.node.label ? LABEL_H : 0) +
    d.props.length * PROP_H +
    rows * ROW_H +
    paramRows(d.node) * PARAM_H +
    PAD_H
  );
}

function pinLabel(pin: string) {
  return pin === SIGNAL_PIN ? "signal" : pin;
}

const ScriptNodeView = memo(function ScriptNodeView({ data, selected }: NodeProps<ScriptFlowNode>) {
  const { node, inputs, outputs, props, color } = data;
  const rows = Math.max(inputs.length, outputs.length);
  const shown = node.params.slice(0, MAX_PARAMS);
  return (
    <div
      className={`${styles.node} ${selected ? styles.nodeSelected : ""}`}
      style={{ width: NODE_W, height: nodeHeight(data), borderTopColor: color }}
    >
      <div className={styles.nodeHead} style={{ height: HEADER_H }}>
        <span className={styles.nodeKind} style={{ color }}>
          {node.kind}
        </span>
        <span className={styles.nodeIndex}>#{node.index}</span>
      </div>
      {node.label && (
        <div className={styles.nodeLabel} style={{ height: LABEL_H }} title={node.label}>
          {node.relay === "emit" ? "⇢ " : node.relay === "listen" ? "⇠ " : ""}
          {node.label}
          {node.global && <span className={styles.globalTag}>global</span>}
        </div>
      )}
      {props.map((p) => (
        <div className={styles.nodeProp} style={{ height: PROP_H }} key={p} title={p}>
          {p}
        </div>
      ))}
      {Array.from({ length: rows }, (_, r) => (
        <div className={styles.pinRow} style={{ height: ROW_H }} key={r}>
          <span className={styles.pinIn}>
            {inputs[r] !== undefined && (
              <>
                <Handle
                  type="target"
                  position={Position.Left}
                  id={`in:${inputs[r]}`}
                  className={styles.handle}
                />
                <span className={inputs[r] === SIGNAL_PIN ? styles.pinSignal : ""}>
                  {pinLabel(inputs[r])}
                </span>
              </>
            )}
          </span>
          <span className={styles.pinOut}>
            {outputs[r] !== undefined && (
              <>
                <span className={outputs[r] === SIGNAL_PIN ? styles.pinSignal : ""}>
                  {pinLabel(outputs[r])}
                </span>
                <Handle
                  type="source"
                  position={Position.Right}
                  id={`out:${outputs[r]}`}
                  className={styles.handle}
                />
              </>
            )}
          </span>
        </div>
      ))}
      {shown.map((p, i) => (
        <div
          className={styles.param}
          style={{ height: PARAM_H }}
          key={i}
          title={`${p.write ? "writes" : "reads"} var ${p.var}${p.name ? ` (${p.name})` : ""}: ${p.value}`}
        >
          <span className={p.write ? styles.paramWrite : styles.paramRead}>{p.write ? "W" : "R"}</span>
          <span className={styles.paramPin}>{p.pin}</span>
          <span className={styles.paramValue}>{p.name ?? p.value}</span>
        </div>
      ))}
      {node.params.length > MAX_PARAMS && (
        <div className={styles.param} style={{ height: PARAM_H }}>
          <span className={styles.paramMore}>+{node.params.length - MAX_PARAMS} more</span>
        </div>
      )}
    </div>
  );
});

const GhostNodeView = memo(function GhostNodeView({ data }: NodeProps<GhostFlowNode>) {
  return (
    <div
      className={styles.ghost}
      style={{ width: GHOST_W, height: GHOST_H, borderLeftColor: data.color }}
      title="In another section — double-click to go there"
    >
      <Handle type="target" position={Position.Left} id="ghost" className={styles.handle} />
      <div className={styles.ghostKind}>
        {data.node.kind} <span className={styles.nodeIndex}>#{data.node.index}</span>
      </div>
      <div className={styles.ghostWhere}>
        {data.node.label ? `${data.node.label} · ` : ""}
        {data.section}
      </div>
      <Handle type="source" position={Position.Right} id="ghost" className={styles.handle} />
    </div>
  );
});

const NODE_TYPES = { script: ScriptNodeView, ghost: GhostNodeView };

// ---------------------------------------------------------------------------
// view building

interface ViewEdge {
  from: number;
  fromPin: string;
  to: number;
  toPin: string;
  kind: "link" | "signal" | "pass";
}

interface View {
  real: number[];
  ghosts: number[];
  edges: ViewEdge[];
  hiddenGhosts: number;
}

/** Contract pass-through chains into direct edges between their ends. */
function contractPassThru(edges: ViewEdge[], passThru: Set<number>): ViewEdge[] {
  const outOf = new Map<number, ViewEdge[]>();
  for (const e of edges) {
    if (!outOf.has(e.from)) outOf.set(e.from, []);
    outOf.get(e.from)!.push(e);
  }
  const ends = (p: number, seen: Set<number>): ViewEdge[] => {
    if (seen.has(p)) return [];
    seen.add(p);
    return (outOf.get(p) ?? []).flatMap((e) => (passThru.has(e.to) ? ends(e.to, seen) : [e]));
  };
  const out: ViewEdge[] = [];
  for (const e of edges) {
    if (passThru.has(e.from)) continue;
    if (!passThru.has(e.to)) {
      out.push(e);
      continue;
    }
    for (const end of ends(e.to, new Set())) {
      out.push({ from: e.from, fromPin: e.fromPin, to: end.to, toPin: end.toPin, kind: "pass" });
    }
  }
  return out;
}

function buildView(
  graph: ArenaGraph,
  section: GraphSection,
  hidePassThru: boolean,
  showOutside: boolean,
): View {
  const inside = new Set(section.nodes);
  const ghosts = new Set<number>();
  let hiddenGhosts = 0;
  const addGhost = (i: number) => {
    if (inside.has(i) || ghosts.has(i)) return true;
    if (!showOutside) return false;
    if (ghosts.size >= MAX_GHOSTS) {
      hiddenGhosts += 1;
      return false;
    }
    ghosts.add(i);
    return true;
  };

  let edges: ViewEdge[] = [];
  for (const l of graph.links) {
    const a = inside.has(l.from);
    const b = inside.has(l.to);
    if (!a && !b) continue;
    if ((a || addGhost(l.from)) && (b || addGhost(l.to))) {
      edges.push({ from: l.from, fromPin: l.from_pin, to: l.to, toPin: l.to_pin, kind: "link" });
    }
  }
  for (const s of graph.signals) {
    for (const e of s.emitters) {
      for (const t of s.listeners) {
        if (e === t || (!inside.has(e) && !inside.has(t))) continue;
        if ((inside.has(e) || addGhost(e)) && (inside.has(t) || addGhost(t))) {
          edges.push({ from: e, fromPin: SIGNAL_PIN, to: t, toPin: SIGNAL_PIN, kind: "signal" });
        }
      }
    }
  }

  let real = section.nodes;
  if (hidePassThru) {
    const pass = new Set(section.nodes.filter((i) => graph.nodes[i].kind === "PassThru"));
    edges = contractPassThru(edges, pass);
    real = real.filter((i) => !pass.has(i));
  }
  return { real, ghosts: [...ghosts], edges, hiddenGhosts };
}

/** Pins every node exposes, whichever section it is drawn in. */
function nodePins(graph: ArenaGraph) {
  const inputs = graph.nodes.map(() => [] as string[]);
  const outputs = graph.nodes.map(() => [] as string[]);
  const add = (list: string[], pin: string) => {
    if (!list.includes(pin)) list.push(pin);
  };
  for (const l of graph.links) {
    add(outputs[l.from], l.from_pin);
    add(inputs[l.to], l.to_pin);
  }
  for (const s of graph.signals) {
    for (const e of s.emitters) add(outputs[e], SIGNAL_PIN);
    for (const t of s.listeners) add(inputs[t], SIGNAL_PIN);
  }
  return { inputs, outputs };
}

/** A couple of scalar prius fields worth showing on the card. */
function propSummary(json: string | undefined): string[] {
  if (!json) return [];
  let obj: Record<string, { Type?: string; Value?: unknown }>;
  try {
    obj = JSON.parse(json.replace(/("Value":\s*)(-?\d{16,})/g, '$1"$2"'));
  } catch {
    return [];
  }
  const out: string[] = [];
  for (const [k, v] of Object.entries(obj)) {
    if (k === "Name" || k === "IsGlobal" || !v || typeof v !== "object") continue;
    const val = v.Value;
    if (typeof val === "string" || typeof val === "number" || typeof val === "boolean") {
      const text = typeof val === "number" ? String(Math.round(val * 1000) / 1000) : String(val);
      out.push(`${k}: ${text}`);
    }
    if (out.length === 2) break;
  }
  return out;
}

// ---------------------------------------------------------------------------
// search

interface SearchField {
  label: string;
  text: string;
  lower: string;
}

interface SearchDoc {
  node: GraphNode;
  head: string;
  fields: SearchField[];
}

interface SearchHit {
  node: GraphNode;
  score: number;
  field: SearchField | null;
}

interface SearchResult {
  hits: SearchHit[];
  all: Set<number>;
  total: number;
}

/** Every scalar in a typed prius blob, keyed by its field path. */
function priusFields(json: string | undefined): { label: string; text: string }[] {
  if (!json) return [];
  let root: unknown;
  try {
    root = JSON.parse(json.replace(/("Value":\s*)(-?\d{16,})/g, '$1"$2"'));
  } catch {
    return [];
  }
  const out: { label: string; text: string }[] = [];
  const walk = (v: unknown, path: string) => {
    if (Array.isArray(v)) {
      v.forEach((x, i) => walk(x, `${path}[${i}]`));
    } else if (v && typeof v === "object") {
      const rec = v as Record<string, unknown>;
      if ("Type" in rec && "Value" in rec) walk(rec.Value, path);
      else for (const [k, x] of Object.entries(rec)) walk(x, path ? `${path}.${k}` : k);
    } else if (v !== null && v !== undefined && v !== "") {
      out.push({ label: path, text: String(v) });
    }
  };
  walk(root, "");
  return out;
}

function buildSearchIndex(graph: ArenaGraph, priuses: PriusRef[]): SearchDoc[] {
  const byId = new Map(priuses.map((p) => [p.id, p.json]));
  const field = (label: string, text: string): SearchField => ({
    label,
    text,
    lower: `${label} ${text}`.toLowerCase(),
  });
  return graph.nodes.map((node) => ({
    node,
    head: `${node.kind} ${node.label ?? ""} #${node.index}`.toLowerCase(),
    fields: [
      ...node.params.map((p) =>
        field(`${p.write ? "writes" : "reads"} ${p.pin}`, [p.name, p.value].filter(Boolean).join(" = ")),
      ),
      ...priusFields(node.prius_id !== null ? byId.get(node.prius_id) : undefined)
        .filter((f) => f.label !== "Name")
        .map((f) => field(f.label, f.text)),
    ],
  }));
}

/** All terms must match; names beat contents, and the open section comes first. */
function searchGraph(index: SearchDoc[], query: string, section: number): SearchResult {
  const q = query.trim().toLowerCase();
  const terms = q.split(/\s+/).filter(Boolean);
  const empty = { hits: [], all: new Set<number>(), total: 0 };
  if (q.length < 2) return empty;

  const hits: SearchHit[] = [];
  for (const doc of index) {
    const matches = terms.every((t) => doc.head.includes(t) || doc.fields.some((f) => f.lower.includes(t)));
    if (!matches) continue;
    const label = (doc.node.label ?? "").toLowerCase();
    const kind = doc.node.kind.toLowerCase();
    const score =
      label === q || kind === q || `#${doc.node.index}` === q
        ? 0
        : label.startsWith(q) || kind.startsWith(q)
          ? 1
          : doc.head.includes(terms[0])
            ? 2
            : 3;
    const field = score === 3 ? (doc.fields.find((f) => f.lower.includes(terms[0])) ?? null) : null;
    hits.push({ node: doc.node, score, field });
  }
  hits.sort(
    (a, b) =>
      a.score - b.score ||
      Number(a.node.section !== section) - Number(b.node.section !== section) ||
      a.node.index - b.node.index,
  );
  return {
    hits: hits.slice(0, MAX_HITS),
    all: new Set(hits.map((h) => h.node.index)),
    total: hits.length,
  };
}

function SearchPalette({
  pos,
  query,
  setQuery,
  found,
  inputRef,
  sectionName,
  onChoose,
  onClose,
}: {
  pos: { x: number; y: number };
  query: string;
  setQuery: (q: string) => void;
  found: SearchResult;
  inputRef: RefObject<HTMLInputElement | null>;
  sectionName: (n: GraphNode) => string;
  onChoose: (index: number) => void;
  onClose: () => void;
}) {
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [inputRef]);

  useEffect(() => setActive(0), [query]);

  useEffect(() => {
    listRef.current?.children[active]?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const onKeyDown = (e: ReactKeyboardEvent) => {
    const n = found.hits.length;
    if (e.key === "ArrowDown" && n) {
      e.preventDefault();
      setActive((a) => (a + 1) % n);
    } else if (e.key === "ArrowUp" && n) {
      e.preventDefault();
      setActive((a) => (a - 1 + n) % n);
    } else if (e.key === "Enter" && n) {
      e.preventDefault();
      onChoose(found.hits[active].node.index);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  return (
    <div className={styles.palette} style={{ left: pos.x, top: pos.y, width: PALETTE_W }}>
      <input
        ref={inputRef}
        className={styles.paletteInput}
        placeholder="Search type, signal, variable, value…"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={onKeyDown}
      />
      <div className={styles.paletteMeta}>
        {query.trim().length < 2
          ? "Type at least 2 characters"
          : found.total === 0
            ? "No matches"
            : `${found.total} match${found.total === 1 ? "" : "es"}${
                found.total > found.hits.length ? ` · first ${found.hits.length}` : ""
              }`}
        <span className={styles.spacer} />
        <kbd>↑↓</kbd> <kbd>Enter</kbd> <kbd>Esc</kbd>
      </div>
      {found.hits.length > 0 && (
        <div className={styles.paletteList} ref={listRef} style={{ maxHeight: PALETTE_H - 70 }}>
          {found.hits.map((h, k) => (
            <button
              key={h.node.index}
              className={`${styles.paletteItem} ${k === active ? styles.paletteActive : ""}`}
              onMouseEnter={() => setActive(k)}
              onClick={() => onChoose(h.node.index)}
            >
              <div className={styles.paletteRow}>
                <span style={{ color: CATEGORY_COLOR[categoryOf(h.node.kind)] }}>{h.node.kind}</span>
                <span className={styles.paletteLabel}>{h.node.label ?? `#${h.node.index}`}</span>
                <span className={styles.resultWhere}>{sectionName(h.node)}</span>
              </div>
              {h.field && (
                <div className={styles.paletteSnippet}>
                  {h.field.label}: {h.field.text}
                </div>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const elk = new ELK();

async function layoutView(
  view: View,
  graph: ArenaGraph,
  pins: ReturnType<typeof nodePins>,
  summaries: Map<number, string[]>,
  sectionLabel: (n: GraphNode) => string,
): Promise<{ nodes: FlowNode[]; edges: Edge[] }> {
  const data = new Map<number, ScriptNodeData>();
  for (const i of view.real) {
    const node = graph.nodes[i];
    data.set(i, {
      node,
      inputs: pins.inputs[i],
      outputs: pins.outputs[i],
      props: summaries.get(i) ?? [],
      color: CATEGORY_COLOR[categoryOf(node.kind)],
    });
  }

  const pinY = (d: ScriptNodeData, row: number) =>
    HEADER_H + (d.node.label ? LABEL_H : 0) + d.props.length * PROP_H + row * ROW_H + ROW_H / 2;

  const children = [
    ...view.real.map((i) => {
      const d = data.get(i)!;
      const ports = [
        ...d.inputs.map((p, r) => ({ id: `${i}:in:${p}`, x: 0, y: pinY(d, r), width: 1, height: 1 })),
        ...d.outputs.map((p, r) => ({
          id: `${i}:out:${p}`,
          x: NODE_W - 1,
          y: pinY(d, r),
          width: 1,
          height: 1,
        })),
      ];
      return {
        id: String(i),
        width: NODE_W,
        height: nodeHeight(d),
        ports,
        layoutOptions: { "elk.portConstraints": "FIXED_POS" },
      };
    }),
    ...view.ghosts.map((i) => ({ id: String(i), width: GHOST_W, height: GHOST_H })),
  ];

  const ghostSet = new Set(view.ghosts);
  const port = (i: number, dir: "in" | "out", pin: string) =>
    ghostSet.has(i) ? String(i) : `${i}:${dir}:${pin}`;
  const elkEdges = view.edges.map((e, k) => ({
    id: `e${k}`,
    sources: [port(e.from, "out", e.fromPin)],
    targets: [port(e.to, "in", e.toPin)],
  }));

  const laid = await elk.layout({
    id: "root",
    layoutOptions: {
      "elk.algorithm": "layered",
      "elk.direction": "RIGHT",
      "elk.layered.spacing.nodeNodeBetweenLayers": "70",
      "elk.spacing.nodeNode": "22",
      "elk.layered.nodePlacement.strategy": "BRANDES_KOEPF",
      "elk.layered.considerModelOrder.strategy": "NODES_AND_EDGES",
    },
    children,
    edges: elkEdges,
  });

  const pos = new Map((laid.children ?? []).map((c) => [Number(c.id), { x: c.x ?? 0, y: c.y ?? 0 }]));
  const nodes: FlowNode[] = [
    ...view.real.map(
      (i): ScriptFlowNode => ({
        id: String(i),
        type: "script",
        position: pos.get(i) ?? { x: 0, y: 0 },
        // Known up front, so off-screen nodes still count for the minimap and framing.
        width: NODE_W,
        height: nodeHeight(data.get(i)!),
        data: data.get(i)!,
      }),
    ),
    ...view.ghosts.map(
      (i): GhostFlowNode => ({
        id: String(i),
        type: "ghost",
        position: pos.get(i) ?? { x: 0, y: 0 },
        width: GHOST_W,
        height: GHOST_H,
        data: {
          node: graph.nodes[i],
          section: sectionLabel(graph.nodes[i]),
          color: CATEGORY_COLOR[categoryOf(graph.nodes[i].kind)],
        },
      }),
    ),
  ];
  const edges: Edge[] = view.edges.map((e, k) => {
    const ghostEnd = ghostSet.has(e.from) || ghostSet.has(e.to);
    return {
      id: `e${k}`,
      source: String(e.from),
      target: String(e.to),
      sourceHandle: ghostSet.has(e.from) ? "ghost" : `out:${e.fromPin}`,
      targetHandle: ghostSet.has(e.to) ? "ghost" : `in:${e.toPin}`,
      className: [
        styles[`edge_${e.kind}`],
        ghostEnd ? styles.edgeOutside : "",
      ].join(" "),
      data: { kind: e.kind },
    };
  });
  return { nodes, edges };
}

// ---------------------------------------------------------------------------
// component

interface Props {
  zonePath: string;
  scriptPriuses: PriusRef[];
  onOpenPrius: (id: number) => void;
}

export default function ArenaGraphView(props: Props) {
  return (
    <ReactFlowProvider>
      <GraphCanvas {...props} />
    </ReactFlowProvider>
  );
}

function GraphCanvas({ zonePath, scriptPriuses, onOpenPrius }: Props) {
  const rf = useReactFlow();
  const [graph, setGraph] = useState<ArenaGraph | null>(null);
  const [error, setError] = useState("");
  const [sectionIdx, setSectionIdx] = useState(0);
  const [hidePassThru, setHidePassThru] = useState(true);
  const [showOutside, setShowOutside] = useState(true);
  const [nodes, setNodes] = useState<FlowNode[]>([]);
  const [edges, setEdges] = useState<Edge[]>([]);
  const [laying, setLaying] = useState(false);
  const [hiddenGhosts, setHiddenGhosts] = useState(0);
  const [selected, setSelected] = useState<number | null>(null);
  const [focus, setFocus] = useState<number | null>(null);
  const [palette, setPalette] = useState<{ x: number; y: number } | null>(null);
  const [query, setQuery] = useState("");
  const canvasRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let live = true;
    setGraph(null);
    setError("");
    setSelected(null);
    setSectionIdx(0);
    invoke<ArenaGraph>("read_arena_graph", { zonePath })
      .then((g) => live && setGraph(g))
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [zonePath]);

  const pins = useMemo(() => (graph ? nodePins(graph) : null), [graph]);
  const summaries = useMemo(() => {
    const byId = new Map(scriptPriuses.map((p) => [p.id, p.json]));
    const out = new Map<number, string[]>();
    for (const n of graph?.nodes ?? []) {
      if (n.prius_id !== null) out.set(n.index, propSummary(byId.get(n.prius_id)));
    }
    return out;
  }, [graph, scriptPriuses]);

  const sectionName = useCallback(
    (n: GraphNode) => graph?.sections[n.section]?.label ?? "?",
    [graph],
  );

  const section = graph?.sections[sectionIdx];

  useEffect(() => {
    if (!graph || !pins || !section) return;
    let live = true;
    setLaying(true);
    const view = buildView(graph, section, hidePassThru, showOutside);
    layoutView(view, graph, pins, summaries, sectionName)
      .then((r) => {
        if (!live) return;
        setNodes(r.nodes);
        setEdges(r.edges);
        setHiddenGhosts(view.hiddenGhosts);
      })
      .catch((e) => live && setError(`layout failed: ${e}`))
      .finally(() => live && setLaying(false));
    return () => {
      live = false;
    };
  }, [graph, pins, section, hidePassThru, showOutside, summaries, sectionName]);

  // Frame the section, or the node a jump asked for, once it is laid out.
  useEffect(() => {
    if (laying || nodes.length === 0) return;
    const id = requestAnimationFrame(() => {
      const target = focus !== null ? nodes.find((n) => n.id === String(focus)) : undefined;
      if (target) {
        rf.fitView({ nodes: [{ id: target.id }], maxZoom: 1.1, duration: 300 });
        setFocus(null);
      } else {
        rf.fitView({ padding: 0.08, duration: 200 });
      }
    });
    return () => cancelAnimationFrame(id);
  }, [nodes, laying]);

  const jumpTo = useCallback(
    (index: number) => {
      if (!graph) return;
      const node = graph.nodes[index];
      setSelected(index);
      if (node.section !== sectionIdx) {
        setFocus(index);
        setSectionIdx(node.section);
      } else {
        const hit = nodes.find((n) => n.id === String(index));
        if (hit) rf.fitView({ nodes: [{ id: hit.id }], maxZoom: 1.1, duration: 300 });
        else setFocus(index);
      }
    },
    [graph, sectionIdx, nodes, rf],
  );

  const pickSection = (i: number) => {
    setSectionIdx(i);
    setSelected(null);
  };

  const searchIndex = useMemo(
    () => (graph ? buildSearchIndex(graph, scriptPriuses) : []),
    [graph, scriptPriuses],
  );
  const found = useMemo(
    () => searchGraph(searchIndex, query, sectionIdx),
    [searchIndex, query, sectionIdx],
  );
  const highlighting = palette !== null && found.all.size > 0;

  /** Open the search at the cursor (right-click) or top-centre (Ctrl+F). */
  const openPalette = useCallback(
    (at?: { clientX: number; clientY: number }) => {
      const rect = canvasRef.current?.getBoundingClientRect();
      if (!rect) return;
      if (palette && !at) {
        searchRef.current?.select();
        return;
      }
      const clamp = (v: number, max: number) => Math.max(8, Math.min(v, max));
      const x = at ? at.clientX - rect.left : (rect.width - PALETTE_W) / 2;
      const y = at ? at.clientY - rect.top : 12;
      setPalette({
        x: clamp(x, rect.width - PALETTE_W - 8),
        y: clamp(y, rect.height - PALETTE_H - 8),
      });
    },
    [palette],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey || e.key.toLowerCase() !== "f") return;
      if (e.target instanceof Element && e.target.closest(".cm-editor")) return;
      e.preventDefault();
      openPalette();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openPalette]);

  const shownNodes = useMemo(
    () =>
      nodes.map((n) => ({
        ...n,
        selected: Number(n.id) === selected,
        className: highlighting
          ? found.all.has(Number(n.id))
            ? styles.matchNode
            : styles.dimNode
          : undefined,
      })),
    [nodes, selected, highlighting, found],
  );
  const shownEdges = useMemo(
    () =>
      selected === null
        ? edges
        : edges.map((e) =>
            e.source === String(selected) || e.target === String(selected)
              ? { ...e, className: `${e.className} ${styles.edgeHot}`, zIndex: 1 }
              : e,
          ),
    [edges, selected],
  );

  if (error) return <p className={styles.error}>{error}</p>;
  if (!graph || !pins) return <p className={styles.muted}>Reading script graph…</p>;

  const waves = graph.sections.map((s, i) => ({ s, i })).filter(({ s }) => s.kind === "wave");
  const chunks = graph.sections.map((s, i) => ({ s, i })).filter(({ s }) => s.kind !== "wave");
  const sel = selected !== null ? graph.nodes[selected] : null;

  return (
    <div className={styles.layout}>
      <aside className={styles.sidebar}>
        <div className={styles.groupTitle}>Waves</div>
        {waves.map(({ s, i }) => (
          <button
            key={s.key}
            className={`${styles.sectionBtn} ${i === sectionIdx ? styles.sectionActive : ""}`}
            onClick={() => pickSection(i)}
          >
            <span className={styles.sectionCount}>{s.nodes.length}</span>
            {s.label}
          </button>
        ))}
        <div className={styles.groupTitle}>Script</div>
        {chunks.map(({ s, i }) => (
          <button
            key={s.key}
            className={`${styles.sectionBtn} ${i === sectionIdx ? styles.sectionActive : ""}`}
            onClick={() => pickSection(i)}
            title={s.label}
          >
            <span className={styles.sectionCount}>{s.nodes.length}</span>
            {s.label}
          </button>
        ))}
        <div className={styles.legend}>
          {(Object.keys(CATEGORY_COLOR) as Category[]).map((c) => (
            <span key={c}>
              <i style={{ background: CATEGORY_COLOR[c] }} />
              {CATEGORY_LABEL[c]}
            </span>
          ))}
        </div>
      </aside>

      <div className={styles.canvasWrap}>
        <div className={styles.toolbar}>
          <strong>{section?.label}</strong>
          <span className={styles.muted}>
            {graph.nodes.length} nodes · {graph.links.length} links · {graph.signals.length} signals ·
            pin names {graph.pins_named}/{graph.pins_total}
          </span>
          <span className={styles.spacer} />
          <button className={styles.searchBtn} onClick={() => openPalette()}>
            Search <kbd>Ctrl F</kbd>
          </button>
          <label className={styles.toggle}>
            <input
              type="checkbox"
              checked={hidePassThru}
              onChange={(e) => setHidePassThru(e.target.checked)}
            />
            Hide pass-throughs
          </label>
          <label className={styles.toggle}>
            <input
              type="checkbox"
              checked={showOutside}
              onChange={(e) => setShowOutside(e.target.checked)}
            />
            Show connections outside
          </label>
        </div>
        <div className={styles.canvas} ref={canvasRef}>
          {laying && <div className={styles.laying}>Laying out…</div>}
          {palette && (
            <SearchPalette
              pos={palette}
              query={query}
              setQuery={setQuery}
              found={found}
              inputRef={searchRef}
              sectionName={sectionName}
              onChoose={(i) => {
                setPalette(null);
                jumpTo(i);
              }}
              onClose={() => setPalette(null)}
            />
          )}
          <ReactFlow
            nodes={shownNodes}
            edges={shownEdges}
            nodeTypes={NODE_TYPES}
            colorMode="dark"
            nodesConnectable={false}
            edgesFocusable={false}
            onlyRenderVisibleElements
            minZoom={0.05}
            maxZoom={2}
            onNodeClick={(_, n) => setSelected(Number(n.id))}
            onNodeDoubleClick={(_, n) => {
              if (n.type === "ghost") jumpTo(Number(n.id));
            }}
            onPaneClick={() => {
              setSelected(null);
              setPalette(null);
            }}
            onPaneContextMenu={(e) => {
              e.preventDefault();
              openPalette(e);
            }}
            onNodeContextMenu={(e) => {
              e.preventDefault();
              openPalette(e);
            }}
          >
            <Background
              id="minor"
              variant={BackgroundVariant.Lines}
              gap={GRID_MINOR}
              lineWidth={1}
              color="#171c24"
            />
            <Background
              id="major"
              variant={BackgroundVariant.Lines}
              gap={GRID_MINOR * 5}
              lineWidth={1}
              color="#232a35"
              bgColor="transparent"
            />
            <Controls showInteractive={false} />
            <MiniMap
              pannable
              zoomable
              nodeColor={(n) => (n.data as { color?: string }).color ?? "#8b949e"}
              maskColor="rgba(15,17,23,0.7)"
            />
          </ReactFlow>
        </div>
        {hiddenGhosts > 0 && (
          <div className={styles.note}>
            {hiddenGhosts} more outside connections not drawn — select a node to list them.
          </div>
        )}
      </div>

      <aside className={styles.inspector}>
        {sel ? (
          <Inspector
            graph={graph}
            node={sel}
            sectionName={sectionName}
            priusJson={
              sel.prius_id !== null
                ? scriptPriuses.find((p) => p.id === sel.prius_id)?.json
                : undefined
            }
            onJump={jumpTo}
            onOpenPrius={onOpenPrius}
          />
        ) : (
          <div className={styles.muted}>
            <p>Click a node to inspect it. Right-click the graph or press Ctrl+F to search.</p>
            <p>
              Solid wires run in order through the pins; dashed purple wires are named signals,
              which the engine pairs by name. Faded cards on the edge are nodes from other
              sections — double-click one to go there.
            </p>
            <p>Read-only for now: edit values in the other tabs.</p>
          </div>
        )}
      </aside>
    </div>
  );
}

function Inspector({
  graph,
  node,
  sectionName,
  priusJson,
  onJump,
  onOpenPrius,
}: {
  graph: ArenaGraph;
  node: GraphNode;
  sectionName: (n: GraphNode) => string;
  priusJson: string | undefined;
  onJump: (i: number) => void;
  onOpenPrius: (id: number) => void;
}) {
  const incoming = graph.links.filter((l) => l.to === node.index);
  const outgoing = graph.links.filter((l) => l.from === node.index);
  const signal = node.signal !== null ? graph.signals[node.signal] : undefined;
  const partners = signal
    ? node.relay === "emit"
      ? signal.listeners
      : node.relay === "listen"
        ? signal.emitters
        : [...signal.emitters, ...signal.listeners].filter((i) => i !== node.index)
    : [];

  const ref = (i: number, pin?: string) => {
    const n = graph.nodes[i];
    return (
      <button className={styles.ref} onClick={() => onJump(i)} title={`#${i} · ${sectionName(n)}`}>
        <span style={{ color: CATEGORY_COLOR[categoryOf(n.kind)] }}>{n.kind}</span>
        {n.label ? ` "${n.label}"` : ` #${i}`}
        {pin ? <span className={styles.refPin}>.{pin}</span> : null}
        {n.section !== node.section && <span className={styles.resultWhere}>{sectionName(n)}</span>}
      </button>
    );
  };

  return (
    <div className={styles.inspect}>
      <h4 style={{ color: CATEGORY_COLOR[categoryOf(node.kind)] }}>{node.kind}</h4>
      <div className={styles.muted}>
        #{node.index} · {sectionName(node)} · {CATEGORY_LABEL[categoryOf(node.kind)]}
      </div>
      {node.label && (
        <div className={styles.inspectLabel}>
          {node.label}
          {node.global && <span className={styles.globalTag}>global</span>}
        </div>
      )}

      {node.relay && (
        <section>
          <h5>
            Signal · {node.relay === "emit" ? "sends" : node.relay === "listen" ? "receives" : node.relay}
          </h5>
          {partners.length > 0 ? (
            partners.map((i) => <div key={i}>{ref(i)}</div>)
          ) : (
            <p className={styles.muted}>
              {node.global
                ? "Global — answered by another zone or the game."
                : "No partner in this zone."}
            </p>
          )}
        </section>
      )}

      {node.params.length > 0 && (
        <section>
          <h5>Variables</h5>
          <table className={styles.paramTable}>
            <tbody>
              {node.params.map((p, i) => (
                <tr key={i}>
                  <td className={p.write ? styles.paramWrite : styles.paramRead}>{p.write ? "W" : "R"}</td>
                  <td>{p.pin}</td>
                  <td className={styles.paramValueCell} title={`var ${p.var}`}>
                    {p.name && <div className={styles.muted}>{p.name}</div>}
                    {p.value}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      <section>
        <h5>Triggered by ({incoming.length})</h5>
        {incoming.length === 0 && <p className={styles.muted}>Nothing in this zone.</p>}
        {incoming.map((l, i) => (
          <div key={i} className={styles.linkRow}>
            {ref(l.from, l.from_pin)} <span className={styles.muted}>→ {l.to_pin}</span>
          </div>
        ))}
      </section>

      <section>
        <h5>Triggers ({outgoing.length})</h5>
        {outgoing.map((l, i) => (
          <div key={i} className={styles.linkRow}>
            <span className={styles.muted}>{l.from_pin} →</span> {ref(l.to, l.to_pin)}
          </div>
        ))}
      </section>

      {priusJson && node.prius_id !== null && (
        <section>
          <h5>
            Properties
            <button className={styles.openBtn} onClick={() => onOpenPrius(node.prius_id as number)}>
              Edit in All Priuses
            </button>
          </h5>
          <pre className={styles.json}>{priusJson}</pre>
        </section>
      )}

      <section className={styles.ids}>
        node {node.node_id}
        <br />
        template {node.template_id}
      </section>
    </div>
  );
}
