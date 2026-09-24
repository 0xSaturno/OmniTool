/**
 * THE SCRIPT PANEL.
 *
 * A zone's gameplay logic is a graph of *actions* — `GateAction`,
 * `DelayAction`, `SetVariableAction` … — wired plug to plug. `zone_script.rs`
 * rebuilds the wiring from the out-plug rows (an out-plug's `+0x04` is the
 * target action index, which reproduces every action's stored in-plug count on
 * 247 073 of 247 073 rows), and this file is the only thing that shows it.
 *
 * A READABLE GRAPH BEATS A PRETTY ONE. There is no node-and-noodle canvas
 * here and there does not need to be: what a person asks of a script is "what
 * is this node, what fires it, and what does it fire", and a list of resolvable
 * links answers that in the narrow inspector column a spring layout could not
 * even fit. Every node id in the detail view is a button that selects that
 * node, so following a chain is clicking.
 *
 * SIZE. The largest shipped graph is 14 707 nodes (`i29_molonoth_gp.zone`), so
 * nothing here renders a full node list into the DOM: the list is WINDOWED to
 * the visible rows plus {@link OVERSCAN} (see {@link NodeList}), and the
 * backend is asked for one connected component at a time through the subgraph
 * picker. The edge lists inside one node's detail are capped at
 * {@link EDGE_CAP} rows each.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { LuArrowDownRight, LuArrowUpRight, LuSearch } from "react-icons/lu";
import type {
  ScriptEdgeDto,
  ScriptGraphDto,
  ScriptNodeDetailDto,
  ScriptNodeDto,
  ScriptVarDto,
  ScriptVarValue,
} from "../types";
import styles from "./ScriptPanel.module.css";

/** Row height of the windowed node list, in px. Must match `.nodeRow`. */
export const ROW_H = 24;
/** Rows rendered above and below the visible window. */
export const OVERSCAN = 8;
/** Edges listed in one direction before the detail view says "+N more". */
export const EDGE_CAP = 40;
/** Nodes the panel will ask the backend for in one go. */
export const NODE_LIMIT = 4000;

export interface ScriptPanelProps {
  graph: ScriptGraphDto | null;
  loading?: boolean;
  error?: string | null;
  /** The component currently being listed; null = every node, up to the cap. */
  subgraph: number | null;
  onSubgraph: (id: number | null) => void;
  selectedNode: number | null;
  onSelectNode: (index: number | null) => void;
  /** `level_zone_script_node` for `selectedNode`. */
  detail: ScriptNodeDetailDto | null;
  detailLoading?: boolean;
  /** Rendered under the detail when the caller also fetched the node's prius. */
  priusSlot?: React.ReactNode;
}

/* ------------------------------------------------------------------ */
/* Windowed node list                                                  */
/* ------------------------------------------------------------------ */

function NodeList({
  nodes,
  selected,
  onSelect,
}: {
  nodes: ScriptNodeDto[];
  selected: number | null;
  onSelect: (i: number) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(280);

  // Measure after layout, never during render: the list is inside a resizable
  // inspector column, so its height is not a constant.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const sync = () => setHeight((h) => (el.clientHeight && el.clientHeight !== h ? el.clientHeight : h));
    sync();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const first = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const count = Math.ceil(height / ROW_H) + OVERSCAN * 2;
  const slice = nodes.slice(first, first + count);

  return (
    <div
      className={styles.nodeList}
      ref={ref}
      onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
    >
      <div style={{ height: nodes.length * ROW_H, position: "relative" }}>
        {slice.map((n, i) => {
          const top = (first + i) * ROW_H;
          return (
            <button
              type="button"
              key={n.index}
              className={`${styles.nodeRow} ${selected === n.index ? styles.nodeRowOn : ""}`}
              style={{ top }}
              onClick={() => onSelect(n.index)}
              title={`uid ${n.uid} · subgraph ${n.subgraph} · prius ${n.priusSize} B`}
            >
              <span className={styles.nodeIndex}>{n.index}</span>
              <span className={styles.nodeType}>
                {n.typeName ?? <span className={styles.unresolved}>#{n.typeNameHash}</span>}
              </span>
              <span className={styles.plugCounts}>
                <span title={`${n.inPlugCount} inbound signals`}>{n.inPlugCount}</span>
                <span className={styles.arrow}>→</span>
                <span title={`${n.outEdges.length} outbound signals`}>{n.outEdges.length}</span>
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* Detail                                                              */
/* ------------------------------------------------------------------ */

function nodeLabel(graph: ScriptGraphDto | null, index: number): string {
  const n = graph?.nodes.find((x) => x.index === index);
  return n?.typeName ? `${index} ${n.typeName}` : `node ${index}`;
}

function EdgeList({
  title,
  edges,
  direction,
  graph,
  onSelect,
}: {
  title: string;
  edges: ScriptEdgeDto[];
  direction: "in" | "out";
  graph: ScriptGraphDto | null;
  onSelect: (i: number) => void;
}) {
  const shown = edges.slice(0, EDGE_CAP);
  return (
    <div className={styles.edgeBlock}>
      <div className={styles.edgeTitle}>
        {direction === "in" ? <LuArrowDownRight /> : <LuArrowUpRight />}
        {title} ({edges.length})
      </div>
      {edges.length === 0 && <div className={styles.none}>none</div>}
      {shown.map((e, i) => {
        const other = direction === "in" ? e.fromNode : e.toNode;
        const mine = direction === "in" ? e.toPlug : e.fromPlug;
        const theirs = direction === "in" ? e.fromPlug : e.toPlug;
        const mineHash = direction === "in" ? e.toPlugHash : e.fromPlugHash;
        const theirsHash = direction === "in" ? e.fromPlugHash : e.toPlugHash;
        return (
          <div key={i} className={styles.edgeRow}>
            <span className={styles.plug} title={`plug hash ${hex32(mineHash)}`}>
              {mine ?? hex32(mineHash)}
            </span>
            <span className={styles.arrow}>{direction === "in" ? "←" : "→"}</span>
            <button
              type="button"
              className={`${styles.nodeLink} ${e.dangling ? styles.dangling : ""}`}
              onClick={() => !e.dangling && onSelect(other)}
              disabled={e.dangling}
              title={e.dangling ? "this edge points outside the zone's action table" : undefined}
            >
              {e.dangling ? `${other} (dangling)` : nodeLabel(graph, other)}
            </button>
            <span className={styles.plug} title={`plug hash ${hex32(theirsHash)}`}>
              {theirs ?? hex32(theirsHash)}
            </span>
          </div>
        );
      })}
      {edges.length > shown.length && (
        <div className={styles.more}>+{edges.length - shown.length} more not shown</div>
      )}
    </div>
  );
}

function hex32(h: number | string): string {
  if (typeof h === "string") return h;
  return `0x${(h >>> 0).toString(16).toUpperCase().padStart(8, "0")}`;
}

function varValueText(v: ScriptVarValue): string {
  switch (v.kind) {
    case "none":
      return "—";
    case "bool":
      return String(v.value);
    case "float":
      return String(v.value);
    case "vec":
      return `(${v.value.map((n) => Math.round(n * 1000) / 1000).join(", ")})`;
    case "string":
      return v.value ?? "(null)";
    case "actors":
      return v.target_name ?? (v.uid !== "0" ? `uid ${v.uid}` : "(unbound)");
    default:
      return v.raw;
  }
}

function varTypeName(v: ScriptVarDto): string {
  return typeof v.ty === "string" ? v.ty : `unknown(${v.ty.unknown})`;
}

/** Plugs the node's TYPE declares but nothing is wired to. */
function unwired(declared: string[], used: (string | null)[]): string[] {
  const seen = new Set(used.filter((x): x is string => !!x));
  return declared.filter((d) => !seen.has(d));
}

function NodeDetail({
  detail,
  graph,
  onSelect,
  priusSlot,
}: {
  detail: ScriptNodeDetailDto;
  graph: ScriptGraphDto | null;
  onSelect: (i: number) => void;
  priusSlot?: React.ReactNode;
}) {
  const n = detail.node;
  const inUnwired = unwired(
    n.declaredInputs,
    detail.inEdges.map((e) => e.toPlug),
  );
  const outUnwired = unwired(
    n.declaredOutputs,
    detail.outEdges.map((e) => e.fromPlug),
  );

  return (
    <div className={styles.detail}>
      <div className={styles.detailHead}>
        <span className={styles.detailIndex}>{n.index}</span>
        <span className={styles.detailType}>{n.typeName ?? `#${n.typeNameHash}`}</span>
      </div>
      <div className={styles.kvGrid}>
        <span className={styles.k}>uid</span>
        <span className={styles.v}>{n.uid}</span>
        <span className={styles.k}>graph uid</span>
        <span className={styles.v} title="the node's id inside the source .scriptgraph, shared by every placement of the same prefab">
          {n.graphUid}
        </span>
        <span className={styles.k}>subgraph</span>
        <span className={styles.v}>{n.subgraph}</span>
        <span className={styles.k}>prius</span>
        <span className={styles.v}>
          {n.priusType ?? "—"}{" "}
          <span className={styles.dim}>
            ({n.priusSize} B at +0x{n.priusOffset.toString(16)})
          </span>
        </span>
      </div>

      <EdgeList
        title="signalled by"
        edges={detail.inEdges}
        direction="in"
        graph={graph}
        onSelect={onSelect}
      />
      <EdgeList
        title="signals"
        edges={detail.outEdges}
        direction="out"
        graph={graph}
        onSelect={onSelect}
      />

      {(inUnwired.length > 0 || outUnwired.length > 0) && (
        <div className={styles.edgeBlock}>
          <div className={styles.edgeTitle}>declared but unwired</div>
          <div className={styles.plugChips}>
            {inUnwired.map((p) => (
              <span key={`i${p}`} className={styles.plugChipIn} title="an input this type declares that nothing signals">
                {p}
              </span>
            ))}
            {outUnwired.map((p) => (
              <span key={`o${p}`} className={styles.plugChipOut} title="an output this type declares that goes nowhere">
                {p}
              </span>
            ))}
          </div>
        </div>
      )}

      {n.vars.length > 0 && (
        <div className={styles.edgeBlock}>
          <div className={styles.edgeTitle}>variables ({n.vars.length})</div>
          {n.vars.map((b, i) => {
            const v = detail.vars[i] ?? null;
            const disagrees = b.authoredDirection && b.authoredDirection !== b.direction;
            return (
              <div key={i} className={styles.varRow}>
                <span className={b.direction === "out" ? styles.dirOut : styles.dirIn}>
                  {b.direction}
                </span>
                <span className={styles.plug} title={`plug hash ${b.plugNameHash}`}>
                  {b.plugName ?? b.plugNameHash}
                </span>
                <span className={styles.varName}>
                  {v ? (v.globalName ?? `local #${v.index}`) : `var ${b.varIndex} (missing)`}
                </span>
                {v && <span className={styles.varType}>{varTypeName(v)}</span>}
                {v && <span className={styles.varValue}>{varValueText(v.value)}</span>}
                {disagrees && (
                  <span
                    className={styles.warn}
                    title={`the file says ${b.direction}, the authored label says ${b.authoredDirection}`}
                  >
                    !
                  </span>
                )}
              </div>
            );
          })}
        </div>
      )}

      {priusSlot}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* The panel                                                           */
/* ------------------------------------------------------------------ */

export default function ScriptPanel({
  graph,
  loading,
  error,
  subgraph,
  onSubgraph,
  selectedNode,
  onSelectNode,
  detail,
  detailLoading,
  priusSlot,
}: ScriptPanelProps) {
  const [query, setQuery] = useState("");

  const nodes = useMemo(() => {
    const all = graph?.nodes ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return all;
    return all.filter(
      (n) =>
        (n.typeName ?? "").toLowerCase().includes(q) ||
        String(n.index) === q ||
        n.uid.toLowerCase().includes(q),
    );
  }, [graph, query]);

  if (loading) return <div className={styles.status}>reading the script graph…</div>;
  if (error) return <div className={styles.statusError}>{error}</div>;
  if (!graph) return null;

  const s = graph.stats;

  return (
    <div className={styles.panel}>
      <div className={styles.stats}>
        <span title="Zone Script Actions rows">{s.nodes.toLocaleString()} nodes</span>
        <span title="signal edges rebuilt from the out-plug rows">
          {s.edges.toLocaleString()} edges
        </span>
        <span title="connected components">{s.subgraphs.toLocaleString()} graphs</span>
        <span title="Zone Script Vars rows">{s.varRows.toLocaleString()} vars</span>
        {s.danglingEdges > 0 && (
          <span className={styles.warn} title="edges whose target is outside the action table">
            {s.danglingEdges} dangling
          </span>
        )}
        {s.inCountMismatch > 0 && (
          <span
            className={styles.warn}
            title="nodes whose stored in-plug count disagrees with the edges rebuilt"
          >
            {s.inCountMismatch} in-count mismatch
          </span>
        )}
      </div>

      <div className={styles.controls}>
        <select
          className={styles.select}
          value={subgraph === null ? "" : String(subgraph)}
          onChange={(e) => onSubgraph(e.target.value === "" ? null : Number(e.target.value))}
        >
          <option value="">
            all {s.nodes.toLocaleString()} nodes (first {NODE_LIMIT.toLocaleString()})
          </option>
          {graph.subgraphs.map((g) => (
            <option key={g.id} value={g.id}>
              #{g.id} · {g.nodeCount} node{g.nodeCount === 1 ? "" : "s"} · {g.label}
            </option>
          ))}
        </select>
        <label className={styles.searchBox}>
          <LuSearch />
          <input
            className={styles.search}
            placeholder="type name, index or uid"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
      </div>

      {graph.truncated && (
        <div className={styles.note}>
          the node list was cut at {NODE_LIMIT.toLocaleString()} — pick a subgraph to see the rest
        </div>
      )}

      <div className={styles.listHead}>
        {nodes.length.toLocaleString()} shown
        {query && ` of ${graph.nodes.length.toLocaleString()}`}
        <span className={styles.dim}> · windowed, {ROW_H}px rows</span>
      </div>

      <NodeList nodes={nodes} selected={selectedNode} onSelect={onSelectNode} />

      {detailLoading && <div className={styles.status}>reading node…</div>}
      {!detailLoading && detail && (
        <NodeDetail
          detail={detail}
          graph={graph}
          onSelect={onSelectNode}
          priusSlot={priusSlot}
        />
      )}
      {!detailLoading && !detail && selectedNode === null && (
        <div className={styles.status}>pick a node to see its plugs and what it signals</div>
      )}
    </div>
  );
}
