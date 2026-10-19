import { createContext, memo, useCallback, useContext, useEffect, useState } from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  MiniMap,
  Position,
  ReactFlow,
  ReactFlowProvider,
  type Connection,
  type Edge,
  type IsValidConnection,
  type Node,
  type NodeChange,
  type NodeProps,
  type OnConnectEnd,
  type XYPosition,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  CENTRE_STYLES,
  SPAWNABLE_STYLES,
  StyleOptions,
  botSummary,
  isTyped,
  parseTypedSafe,
  type ArenaActorAsset,
  type ArenaEditApi,
  type ArenaPrius,
  type ArenaSpawnBinding,
  type ArenaSpawnTarget,
  type ArenaSpawner,
  type ArenaWave,
  type MessageDraft,
  type MessageStyle,
  type MessageWhen,
  type Typed,
} from "./arenaModel";
import styles from "./ArenaFlowView.module.css";

const ApiContext = createContext<ArenaEditApi | null>(null);
const useApi = () => useContext(ApiContext) as ArenaEditApi;

// ---------------------------------------------------------------------------
// geometry — fixed row heights so the layout knows every node's size

const COL_MSG = 0;
const COL_WAVE = 300;
const COL_SPAWNER = 610;
const COL_RES = 960;
const W_MSG = 260;
const W_WAVE = 250;
const W_SPAWNER = 300;
const W_RES = 280;

const HEAD_H = 28;
const ROW_H = 26;
const PAD_H = 10;
const BLOCK_GAP = 48;
const STACK_GAP = 14;

/** Spawner prius fields worth editing inline, with plain labels. */
const PACING_FIELDS: [string, string][] = [
  ["MaxSimultaneousSpawns", "Max at once"],
  ["SpawnIntervalMin", "Interval min (s)"],
  ["SpawnIntervalMax", "Interval max (s)"],
  ["InitialSpawnDelayMin", "First spawn min (s)"],
  ["InitialSpawnDelayMax", "First spawn max (s)"],
  ["MinDistanceFromPlayers", "Min player distance"],
];

const STYLE_COLOR: Record<string, string> = {
  volume: "#3fb950",
  portal: "#c58af9",
  animclue: "#e3b341",
  static: "#d29922",
  other: "#8b949e",
};

interface PacingField {
  key: string;
  label: string;
  typed: Typed;
}

interface SpawnerModel {
  spawner: ArenaSpawner;
  prius: ArenaPrius | undefined;
  fields: PacingField[];
}

type StartData = { kind: "start" };
type VictoryData = { kind: "victory" };
type WaveData = { kind: "wave"; wave: ArenaWave; spawners: number };
type SpawnerData = { kind: "spawner"; model: SpawnerModel; wave: number };
type EnemyData = { kind: "enemy"; asset: ArenaActorAsset; waves: number[] };
type PointData = { kind: "point"; target: ArenaSpawnTarget };
type MessageData = { kind: "message"; draft: MessageDraft };

type FlowData = (
  | StartData
  | VictoryData
  | WaveData
  | SpawnerData
  | EnemyData
  | PointData
  | MessageData
) &
  Record<string, unknown>;

const isInteger = (t: string) => /^U?Int/.test(t);

function pacingFields(prius: ArenaPrius | undefined): PacingField[] {
  if (!prius) return [];
  const json = parseTypedSafe(prius.json);
  return PACING_FIELDS.flatMap(([key, label]) => {
    const typed = json[key];
    return isTyped(typed) && typeof typed.Value === "number" ? [{ key, label, typed }] : [];
  });
}

function spawnerHeight(m: SpawnerModel) {
  const rows = 1 + m.fields.length + 1 + Math.max(1, m.spawner.locations.length);
  return HEAD_H + 18 + rows * ROW_H + PAD_H;
}

const WAVE_H = HEAD_H + 20 + 30 + 2 * ROW_H + PAD_H;
const START_H = HEAD_H + 32 + 22 + PAD_H;
const VICTORY_H = HEAD_H + 32 + 30 + PAD_H;
const MESSAGE_H = HEAD_H + 32 + ROW_H + PAD_H;
const ENEMY_H = HEAD_H + 32 + 24 + PAD_H;
const POINT_H = HEAD_H + 22 + PAD_H;

function assetOf(api: ArenaEditApi, s: ArenaSpawner): ArenaActorAsset | undefined {
  const id = api.templateId(s);
  const assets = api.data.actor_assets;
  return (
    (id ? assets.find((a) => a.instance_ids.includes(id)) : undefined) ??
    assets.find((a) => a.instances.includes(s.template))
  );
}

function targetOf(api: ArenaEditApi, b: ArenaSpawnBinding): ArenaSpawnTarget | undefined {
  const id = api.bindingId(b);
  return (
    api.data.spawn_targets.find((t) => t.id === id) ??
    (b.kind === "actor" || b.kind === "group"
      ? { id, kind: b.kind, label: b.label, asset: b.asset, count: 1, style: b.style }
      : undefined)
  );
}

// ---------------------------------------------------------------------------
// layout

function buildFlow(api: ArenaEditApi) {
  const { data } = api;
  const nodes: Node<FlowData>[] = [];
  const edges: Edge[] = [];
  const priusById = new Map(data.script_priuses.map((p) => [p.id, p]));

  // Copies are real waves in `data` (previewed); each plays straight after its source.
  const copyNumbers = new Set(api.clones.map((c) => c.new_number));
  const originals = data.waves
    .filter((w) => !copyNumbers.has(w.number))
    .sort((a, b) => a.number - b.number);
  const timeline: { kind: "wave"; wave: ArenaWave }[] = [];
  for (const wave of originals) {
    timeline.push({ kind: "wave", wave });
    for (const c of api.clones) {
      const copy = c.source === wave.number && data.waves.find((w) => w.number === c.new_number);
      if (copy) timeline.push({ kind: "wave", wave: copy });
    }
  }

  let y = 0;
  nodes.push({ id: "start", type: "start", position: { x: COL_WAVE, y }, width: W_WAVE, height: START_H, data: { kind: "start" } });
  let prev = "start";
  y += START_H + BLOCK_GAP;

  const users = new Map<string, number[]>();
  const use = (id: string, at: number) => {
    if (!users.has(id)) users.set(id, []);
    users.get(id)!.push(at);
  };
  const enemyWaves = new Map<number, Set<number>>();

  for (const entry of timeline) {
    const number = entry.wave.number;
    const id = `wave:${number}`;
    const existing = entry.wave.messages;
    const top = y;

    nodes.push({
      id,
      type: "wave",
      position: { x: COL_WAVE, y },
      width: W_WAVE,
      height: WAVE_H,
      data: { kind: "wave", wave: entry.wave, spawners: entry.wave.spawners.length },
    });
    edges.push(flowEdge(prev, id));
    prev = id;

    let sy = top;
    for (const spawner of entry.wave.spawners) {
      const prius = spawner.prius_id !== null ? priusById.get(spawner.prius_id) : undefined;
      const model: SpawnerModel = { spawner, prius, fields: pacingFields(prius) };
      const sid = `spawner:${spawner.node}`;
      const h = spawnerHeight(model);
      nodes.push({
        id: sid,
        type: "spawner",
        position: { x: COL_SPAWNER, y: sy },
        width: W_SPAWNER,
        height: h,
        data: { kind: "spawner", model, wave: number },
      });
      edges.push({
        id: `own:${id}->${sid}`,
        source: id,
        sourceHandle: "spawners",
        target: sid,
        targetHandle: "wave",
        reconnectable: false,
        className: styles.edgeOwn,
      });

      const asset = assetOf(api, spawner);
      if (asset) {
        const eid = `enemy:${asset.index}`;
        use(eid, sy + HEAD_H);
        if (!enemyWaves.has(asset.index)) enemyWaves.set(asset.index, new Set());
        enemyWaves.get(asset.index)!.add(number);
        const edited = api.templateId(spawner) !== spawner.template_id;
        edges.push({
          id: `enemy:${sid}`,
          source: sid,
          sourceHandle: "enemy",
          target: eid,
          targetHandle: "in",
          className: `${styles.edgeEnemy} ${edited ? styles.edgeEdited : ""}`,
        });
      }
      for (const b of spawner.locations) {
        const target = targetOf(api, b);
        if (!target) continue;
        const pid = `point:${target.id}`;
        use(pid, sy + HEAD_H);
        edges.push({
          id: `point:${sid}:${b.var}`,
          source: sid,
          sourceHandle: `from:${b.var}`,
          target: pid,
          targetHandle: "in",
          className: `${styles.edgePoint} ${api.bindingId(b) !== b.id ? styles.edgeEdited : ""}`,
        });
      }
      sy += h + STACK_GAP;
    }

    let my = top;
    for (const draft of api.messageRows(number, existing)) {
      const mid = `msg:${draft.key}`;
      nodes.push({
        id: mid,
        type: "message",
        position: { x: COL_MSG, y: my },
        width: W_MSG,
        height: MESSAGE_H,
        data: { kind: "message", draft },
      });
      edges.push({
        id: `msg:${id}:${draft.key}`,
        source: id,
        sourceHandle: draft.when,
        target: mid,
        targetHandle: "in",
        className: `${styles.edgeMessage} ${api.isDirtyMessage(draft) ? styles.edgeEdited : ""}`,
      });
      my += MESSAGE_H + STACK_GAP;
    }

    y = Math.max(top + WAVE_H, sy - STACK_GAP, my - STACK_GAP) + BLOCK_GAP;
  }

  nodes.push({ id: "victory", type: "victory", position: { x: COL_WAVE, y }, width: W_WAVE, height: VICTORY_H, data: { kind: "victory" } });
  edges.push(flowEdge(prev, "victory"));

  // Enemies and spawn points sit beside the spawners that use them.
  const resources: { id: string; want: number; h: number; node: Node<FlowData> }[] = [];
  const enemyAssets = data.actor_assets.filter(
    (a) => a.is_enemy || users.has(`enemy:${a.index}`),
  );
  for (const asset of enemyAssets) {
    const id = `enemy:${asset.index}`;
    const at = users.get(id);
    resources.push({
      id,
      want: at ? at.reduce((s, v) => s + v, 0) / at.length : Number.MAX_SAFE_INTEGER,
      h: ENEMY_H,
      node: {
        id,
        type: "enemy",
        position: { x: COL_RES, y: 0 },
        width: W_RES,
        height: ENEMY_H,
        data: { kind: "enemy", asset, waves: [...(enemyWaves.get(asset.index) ?? [])].sort((a, b) => a - b) },
      },
    });
  }
  const seenPoints = new Set<string>();
  for (const n of nodes) {
    if (n.data.kind !== "spawner") continue;
    for (const b of n.data.model.spawner.locations) {
      const target = targetOf(api, b);
      if (!target || seenPoints.has(target.id)) continue;
      seenPoints.add(target.id);
      const id = `point:${target.id}`;
      const at = users.get(id) ?? [0];
      resources.push({
        id,
        want: at.reduce((s, v) => s + v, 0) / at.length,
        h: POINT_H,
        node: {
          id,
          type: "point",
          position: { x: COL_RES, y: 0 },
          width: W_RES,
          height: POINT_H,
          data: { kind: "point", target },
        },
      });
    }
  }
  resources.sort((a, b) => a.want - b.want);
  let floor = -Infinity;
  for (const r of resources) {
    const ry = Math.max(r.want === Number.MAX_SAFE_INTEGER ? floor : r.want - r.h / 2, floor);
    r.node.position = { x: COL_RES, y: ry };
    floor = ry + r.h + STACK_GAP;
    nodes.push(r.node);
  }

  return { nodes, edges };
}

function flowEdge(from: string, to: string): Edge {
  return {
    id: `flow:${from}->${to}`,
    source: from,
    sourceHandle: "next",
    target: to,
    targetHandle: "prev",
    reconnectable: false,
    type: "smoothstep",
    animated: true,
    className: styles.edgeFlow,
  };
}

// ---------------------------------------------------------------------------
// nodes

function Head({ title, sub, color }: { title: string; sub?: string; color: string }) {
  return (
    <div className={styles.head} style={{ height: HEAD_H }}>
      <span className={styles.title} style={{ color }}>
        {title}
      </span>
      {sub && <span className={styles.sub}>{sub}</span>}
    </div>
  );
}

function NumberField({
  value,
  edited,
  step,
  onChange,
}: {
  value: number;
  edited: boolean;
  step?: number;
  onChange: (v: number) => void;
}) {
  return (
    <input
      className={`nodrag ${styles.num} ${edited ? styles.dirty : ""}`}
      type="number"
      min={0}
      step={step ?? 1}
      value={value}
      onChange={(e) => onChange(e.target.value === "" ? 0 : Number(e.target.value))}
    />
  );
}

const StartNode = memo(function StartNode() {
  const api = useApi();
  const t = api.titleText;
  return (
    <div className={`${styles.node} ${styles.nodeFlow}`}>
      <Head title="Challenge start" color="#3fb950" />
      <div className={styles.body}>
        {t ? (
          <input
            className={`nodrag ${styles.text} ${t.edited ? styles.dirty : ""}`}
            value={t.value}
            placeholder="Title card text"
            title="Title card — a loc key, or your own text (\n for a new line)"
            onChange={(e) => t.set(e.target.value)}
          />
        ) : (
          <span className={styles.muted}>No title card in this zone.</span>
        )}
        <span className={styles.muted}>Intro and countdown play, then wave 1.</span>
      </div>
      <Handle type="source" position={Position.Bottom} id="next" className={styles.handle} />
    </div>
  );
});

const VictoryNode = memo(function VictoryNode() {
  const api = useApi();
  const v = api.victory;
  return (
    <div className={`${styles.node} ${styles.nodeFlow}`}>
      <Handle type="target" position={Position.Top} id="prev" className={styles.handle} />
      <Head title="Victory" color="#e3b341" />
      <div className={styles.body}>
        {v ? (
          <>
            <input
              className={`nodrag ${styles.text} ${v.edited ? styles.dirty : ""}`}
              value={v.text}
              placeholder="Victory! — stock text; type to replace"
              onChange={(e) => v.setText(e.target.value)}
            />
            <select
              className={`nodrag ${styles.select}`}
              value={v.style}
              onChange={(e) => v.setStyle(e.target.value as MessageStyle)}
            >
              <StyleOptions />
            </select>
          </>
        ) : (
          <span className={styles.muted}>Stock victory banner.</span>
        )}
      </div>
    </div>
  );
});

function MessagePorts({ can }: { can: { start: boolean; cleared: boolean } }) {
  return (
    <>
      {(["start", "cleared"] as MessageWhen[]).map((when) => (
        <div className={styles.portRow} style={{ height: ROW_H }} key={when}>
          {can[when] ? (
            <>
              <Handle
                type="source"
                position={Position.Left}
                id={when}
                className={`${styles.handle} ${styles.handleMsg}`}
              />
              <span className={styles.portLabel}>
                {when === "start" ? "On start" : "On cleared"}
              </span>
            </>
          ) : (
            <span className={styles.muted}>
              {when === "start" ? "On start" : "On cleared"} — no hook
            </span>
          )}
        </div>
      ))}
    </>
  );
}

const WaveNode = memo(function WaveNode({ data }: NodeProps<Node<WaveData>>) {
  const api = useApi();
  const [copies, setCopies] = useState(1);
  const n = data.wave.number;
  const total = data.wave.spawners.reduce((s, sp) => s + api.numSpawns(sp), 0);
  const can = { start: data.wave.can_message_start, cleared: data.wave.can_message_cleared };
  const copyOf = api.cloneSource(n);
  const blocker = api.cloneBlocker(n);
  return (
    <div className={`${styles.node} ${copyOf !== undefined ? styles.nodeClone : styles.nodeWave}`}>
      <Handle type="target" position={Position.Top} id="prev" className={styles.handle} />
      <Head
        title={`Wave ${n}`}
        sub={copyOf !== undefined ? `copy of ${copyOf} · ${total} enemies` : `${total} enemies`}
        color="#58a6ff"
      />
      <div className={styles.body}>
        <div className={styles.inline}>
          <span className={styles.muted}>
            {data.spawners} spawner{data.spawners === 1 ? "" : "s"}
          </span>
          {api.isNewestClone(n) && (
            <button
              className={`nodrag ${styles.btn} ${styles.btnRight}`}
              onClick={api.removeLastClone}
              title="Remove this copy (the newest copy is removed first)"
            >
              Remove copy
            </button>
          )}
        </div>
        <div className={styles.inline} title={blocker ? `Can't duplicate: ${blocker}` : undefined}>
          <span className={styles.muted}>copies</span>
          <input
            className={`nodrag ${styles.num} ${styles.numSmall}`}
            type="number"
            min={1}
            max={20}
            value={copies}
            disabled={blocker !== null}
            onChange={(e) => setCopies(Math.max(1, Math.min(20, Number(e.target.value) || 1)))}
          />
          <button
            className={`nodrag ${styles.btn}`}
            onClick={() => api.queueClone(n, copies)}
            disabled={blocker !== null || api.previewing}
            title={
              blocker
                ? `Can't duplicate: ${blocker}`
                : "Duplicate this wave (Shift+D); copies run straight after it and are editable right away"
            }
          >
            Duplicate
          </button>
        </div>
        <MessagePorts can={can} />
      </div>
      <Handle type="source" position={Position.Right} id="spawners" className={styles.handle} />
      <Handle type="source" position={Position.Bottom} id="next" className={styles.handle} />
    </div>
  );
});

const SpawnerNode = memo(function SpawnerNode({ data }: NodeProps<Node<SpawnerData>>) {
  const api = useApi();
  const { spawner, prius, fields } = data.model;
  const asset = assetOf(api, spawner);
  const enemies = api.data.actor_assets.filter((a) => a.is_enemy || a === asset);
  const count = api.numSpawns(spawner);
  const shared = prius && prius.owners.length > 1 ? prius.owners.length : 0;
  return (
    <div className={`${styles.node} ${styles.nodeSpawner}`}>
      <Handle type="target" position={Position.Left} id="wave" className={styles.handle} />
      <Head title="Spawner" sub={`#${spawner.node}`} color="#f0883e" />
      <div className={styles.meta} style={{ height: 18 }}>
        wave {data.wave}
        {shared ? ` · pacing shared by ${shared} spawners` : ""}
      </div>
      <div className={styles.row} style={{ height: ROW_H }}>
        <span>Enemies</span>
        {spawner.num_spawns_var !== null ? (
          <NumberField
            value={count}
            edited={count !== (spawner.num_spawns ?? 0)}
            onChange={(v) => api.setNumSpawns(spawner, v)}
          />
        ) : (
          <span className={styles.muted}>—</span>
        )}
      </div>
      {fields.map((f) => {
        const value = api.priusValue("script", prius!.id, [f.key], f.typed.Value) as number;
        return (
          <div className={styles.row} style={{ height: ROW_H }} key={f.key}>
            <span>{f.label}</span>
            <NumberField
              value={value}
              edited={value !== f.typed.Value}
              step={isInteger(f.typed.Type) ? 1 : 0.05}
              onChange={(v) =>
                api.setPrius("script", prius!.id, [f.key], isInteger(f.typed.Type) ? Math.round(v) : v)
              }
            />
          </div>
        );
      })}
      <div className={`${styles.row} ${styles.outRow}`} style={{ height: ROW_H }}>
        <span>Enemy</span>
        <select
          className={`nodrag ${styles.select} ${
            api.templateId(spawner) !== spawner.template_id ? styles.dirty : ""
          }`}
          value={asset?.index ?? ""}
          title="Which enemy this spawner builds — untested in game when changed"
          onChange={(e) => {
            const a = api.data.actor_assets.find((x) => x.index === Number(e.target.value));
            if (a?.instance_ids[0]) api.setTemplate(spawner, a.instance_ids[0]);
          }}
          disabled={spawner.template_var === null}
        >
          {!asset && <option value="">{spawner.template || "unknown"}</option>}
          {enemies.map((a) => (
            <option key={a.index} value={a.index}>
              {a.instances[0] ?? `asset ${a.index}`}
            </option>
          ))}
        </select>
        {spawner.template_var !== null && (
          <Handle
            type="source"
            position={Position.Right}
            id="enemy"
            className={`${styles.handle} ${styles.handleEnemy}`}
          />
        )}
      </div>
      {spawner.locations.length === 0 && (
        <div className={styles.row} style={{ height: ROW_H }}>
          <span>From</span>
          <span className={styles.muted}>—</span>
        </div>
      )}
      {spawner.locations.map((b) => (
        <FromRow key={b.var} binding={b} />
      ))}
    </div>
  );
});

function FromRow({ binding: b }: { binding: ArenaSpawnBinding }) {
  const api = useApi();
  if (b.kind === "unresolved") {
    return (
      <div className={styles.row} style={{ height: ROW_H }}>
        <span>From</span>
        <span className={styles.muted} title={b.via}>
          filled at runtime
        </span>
      </div>
    );
  }
  const current = api.bindingId(b);
  const target = targetOf(api, b);
  const spawnable = SPAWNABLE_STYLES.includes(target?.style ?? b.style);
  const options = api.data.spawn_targets.filter(
    (t) => t.kind === b.kind && SPAWNABLE_STYLES.includes(t.style),
  );
  return (
    <div className={`${styles.row} ${styles.outRow}`} style={{ height: ROW_H }}>
      <span>From</span>
      <select
        className={`nodrag ${styles.select} ${current !== b.id ? styles.dirty : ""} ${
          spawnable ? "" : styles.warn
        }`}
        value={current}
        title={
          spawnable
            ? undefined
            : "A position marker, not a spawn point — the game falls back to the portals. Pick a spawn volume."
        }
        onChange={(e) => api.setBinding(b, e.target.value)}
      >
        {!options.some((t) => t.id === current) && (
          <option value={current}>
            {target?.label ?? b.label}
            {spawnable ? "" : " (not a spawn point)"}
          </option>
        )}
        {options.map((t) => (
          <option key={t.id} value={t.id}>
            {t.kind === "group" ? `${t.label} [${t.count}]` : t.label} · {t.style}
          </option>
        ))}
      </select>
      <Handle
        type="source"
        position={Position.Right}
        id={`from:${b.var}`}
        className={`${styles.handle} ${styles.handlePoint}`}
      />
    </div>
  );
}

const EnemyNode = memo(function EnemyNode({ data }: NodeProps<Node<EnemyData>>) {
  const api = useApi();
  const { asset, waves } = data;
  const path = api.assetPath(asset);
  const stats = asset.prius_ids
    .map((id) => api.data.actor_priuses.find((b) => b.id === id))
    .filter((b): b is ArenaPrius => Boolean(b))
    .flatMap(botSummary);
  return (
    <div className={`${styles.node} ${styles.nodeEnemy} ${waves.length ? "" : styles.unused}`}>
      <Handle type="target" position={Position.Left} id="in" className={`${styles.handle} ${styles.handleEnemy}`} />
      <Head
        title={asset.instances[0] ?? `asset ${asset.index}`}
        sub={waves.length ? `wave ${waves.join(", ")}` : "unused"}
        color="#ff7b72"
      />
      <div className={styles.body}>
        <input
          className={`nodrag ${styles.text} ${styles.mono} ${path !== asset.path ? styles.dirty : ""}`}
          value={path}
          spellCheck={false}
          title={`Actor asset — editing swaps it everywhere it is used\n${path}`}
          onChange={(e) => api.setAssetPath(asset, e.target.value)}
        />
        <div className={styles.chips}>
          {stats.map((s) => (
            <span className={styles.chip} key={s}>
              {s}
            </span>
          ))}
          {asset.instances.length > 1 && <span className={styles.muted}>×{asset.instances.length}</span>}
        </div>
      </div>
    </div>
  );
});

const PointNode = memo(function PointNode({ data }: NodeProps<Node<PointData>>) {
  const t = data.target;
  const color = STYLE_COLOR[t.style] ?? STYLE_COLOR.other;
  return (
    <div className={`${styles.node} ${styles.nodePoint}`} style={{ borderLeftColor: color }}>
      <Handle type="target" position={Position.Left} id="in" className={`${styles.handle} ${styles.handlePoint}`} />
      <Head
        title={t.kind === "group" ? `${t.label} [${t.count}]` : t.label}
        sub={t.kind}
        color={color}
      />
      <div
        className={`${styles.meta} ${SPAWNABLE_STYLES.includes(t.style) ? "" : styles.metaWarn}`}
        style={{ height: 22 }}
      >
        {t.style === "portal"
          ? "rift portal — enemy needs a portal entry"
          : t.style === "volume"
            ? "spawn volume — enemy just appears"
            : t.style === "animclue"
              ? "anim clue — scripted entry"
              : "not a spawn point — enemies fall back to portals"}
      </div>
    </div>
  );
});

const MessageNode = memo(function MessageNode({ data }: NodeProps<Node<MessageData>>) {
  const api = useApi();
  const d = data.draft;
  const clash = d.when === "cleared" && CENTRE_STYLES.includes(d.style);
  return (
    <div className={`${styles.node} ${styles.nodeMessage}`}>
      <div className={styles.head} style={{ height: HEAD_H }}>
        <select
          className={`nodrag ${styles.select} ${clash ? styles.warn : ""}`}
          value={d.style}
          title={clash ? "Collides with the stock “Wave complete” banner — the help box doesn't" : "Style"}
          onChange={(e) => api.updateMessage(d, { style: e.target.value as MessageStyle })}
        >
          <StyleOptions />
        </select>
        <button className={`nodrag ${styles.remove}`} onClick={() => api.removeMessage(d)} title="Remove message">
          ✕
        </button>
      </div>
      <div className={styles.body}>
        <input
          className={`nodrag ${styles.text} ${api.isDirtyMessage(d) ? styles.dirty : ""}`}
          value={d.text}
          maxLength={160}
          placeholder={d.node === undefined ? "Text to show…" : "Empty removes this message"}
          onChange={(e) => api.updateMessage(d, { text: e.target.value })}
        />
        <div className={styles.inline}>
          <span className={styles.muted}>delay</span>
          <input
            className={`nodrag ${styles.num} ${styles.numSmall}`}
            type="number"
            min={0}
            step={0.5}
            value={d.delay}
            onChange={(e) => api.updateMessage(d, { delay: Math.max(0, Number(e.target.value) || 0) })}
          />
          <span className={styles.muted}>shown</span>
          <input
            className={`nodrag ${styles.num} ${styles.numSmall}`}
            type="number"
            min={0.5}
            step={0.5}
            value={d.duration}
            onChange={(e) =>
              api.updateMessage(d, { duration: Math.max(0.5, Number(e.target.value) || 4) })
            }
          />
          <span className={styles.muted}>s</span>
        </div>
      </div>
      <Handle type="target" position={Position.Right} id="in" className={`${styles.handle} ${styles.handleMsg}`} />
    </div>
  );
});

const NODE_TYPES = {
  start: StartNode,
  victory: VictoryNode,
  wave: WaveNode,
  spawner: SpawnerNode,
  enemy: EnemyNode,
  point: PointNode,
  message: MessageNode,
};

// ---------------------------------------------------------------------------
// view

export default function ArenaFlowView({ api }: { api: ArenaEditApi }) {
  return (
    <ApiContext.Provider value={api}>
      <ReactFlowProvider>
        <FlowCanvas api={api} />
      </ReactFlowProvider>
    </ApiContext.Provider>
  );
}

function FlowCanvas({ api }: { api: ArenaEditApi }) {
  const [moved, setMoved] = useState<Record<string, XYPosition>>({});
  const [selected, setSelected] = useState<string | null>(null);

  const flow = buildFlow(api);
  const byId = new Map(flow.nodes.map((n) => [n.id, n]));
  // Open on the start of the timeline; the wheel scrolls down it.
  const firstWave = flow.nodes.find((n) => n.type === "wave");
  const firstBlock = [
    { id: "start" },
    ...flow.edges
      .filter((e) => firstWave && e.source === firstWave.id)
      .map((e) => ({ id: e.target })),
    ...(firstWave ? [{ id: firstWave.id }] : []),
  ];
  const nodes = flow.nodes.map((n) => ({
    ...n,
    position: moved[n.id] ?? n.position,
    selected: n.id === selected,
  }));

  const onNodesChange = useCallback((changes: NodeChange<Node<FlowData>>[]) => {
    for (const c of changes) {
      if (c.type === "position" && c.position) {
        const pos = c.position;
        setMoved((prev) => ({ ...prev, [c.id]: pos }));
      } else if (c.type === "select") {
        setSelected((cur) => (c.selected ? c.id : cur === c.id ? null : cur));
      }
    }
  }, []);

  const waveOf = (nodeId: string): number | null => {
    const n = byId.get(nodeId);
    if (n?.data.kind === "wave") return (n.data as WaveData).wave.number;
    return null;
  };

  /** What a wire from `source` to `target` would mean, or null if nothing. */
  const interpret = (c: Connection | Edge) => {
    const src = byId.get(c.source);
    const dst = byId.get(c.target);
    if (!src || !dst) return null;
    const handle = c.sourceHandle ?? "";
    if ((handle === "start" || handle === "cleared") && dst.data.kind === "message") {
      const wave = waveOf(src.id);
      if (wave === null) return null;
      return () => api.moveMessage((dst.data as MessageData).draft, wave, handle);
    }
    if (src.data.kind !== "spawner") return null;
    const spawner = (src.data as SpawnerData).model.spawner;
    if (handle === "enemy" && dst.data.kind === "enemy") {
      const id = (dst.data as EnemyData).asset.instance_ids[0];
      return id ? () => api.setTemplate(spawner, id) : null;
    }
    if (handle.startsWith("from:") && dst.data.kind === "point") {
      const b = spawner.locations.find((x) => `from:${x.var}` === handle);
      const t = (dst.data as PointData).target;
      if (!b || b.kind !== t.kind || !SPAWNABLE_STYLES.includes(t.style)) return null;
      return () => api.setBinding(b, t.id);
    }
    return null;
  };

  // Shift+D duplicates the selected wave, as in Blender.
  const selectedWave = selected ? waveOf(selected) : null;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.shiftKey || e.ctrlKey || e.altKey || e.key.toLowerCase() !== "d") return;
      if (e.target instanceof Element && e.target.closest("input, select, textarea")) return;
      if (selectedWave === null || api.cloneBlocker(selectedWave) !== null || api.previewing) return;
      e.preventDefault();
      api.queueClone(selectedWave, 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedWave, api]);

  const isValidConnection: IsValidConnection = (c) => interpret(c) !== null;

  const onConnect = (c: Connection) => interpret(c)?.();

  // A wire dropped anywhere on a node connects to it, not only on its socket;
  // a wave's message wire dropped on empty canvas adds a message there.
  const onConnectEnd: OnConnectEnd = (event, state) => {
    stopWiring();
    if (state.isValid) return;
    const from = state.fromNode?.id;
    const handle = state.fromHandle?.id;
    if (!from || !handle || state.fromHandle?.type !== "source") return;
    const point = "changedTouches" in event ? event.changedTouches[0] : event;
    const hit = document
      .elementFromPoint(point.clientX, point.clientY)
      ?.closest(".react-flow__node")
      ?.getAttribute("data-id");
    if (hit) {
      interpret({ source: from, sourceHandle: handle, target: hit, targetHandle: "in" })?.();
      return;
    }
    const wave = waveOf(from);
    if (wave !== null && (handle === "start" || handle === "cleared")) api.addMessage(wave, handle);
  };

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <span className={styles.legend}>
          <i style={{ background: "#58a6ff" }} /> waves
          <i style={{ background: "#f0883e" }} /> spawners
          <i style={{ background: "#ff7b72" }} /> enemies
          <i style={{ background: "#c58af9" }} /> spawn points
          <i style={{ background: "#39c5cf" }} /> messages
        </span>
        <span className={styles.hint}>
          Drag a wire's end to rewire it · drag from On start / On cleared into empty space to add a
          message · Shift+D duplicates the selected wave
        </span>
        <span className={styles.spacer} />
        {api.previewing && <span className={styles.previewing}>Preparing wave copy…</span>}
        {Object.keys(moved).length > 0 && (
          <button className={styles.btn} onClick={() => setMoved({})}>
            Reset layout
          </button>
        )}
      </div>
      <div className={styles.canvas}>
        <ReactFlow
          nodes={nodes}
          edges={flow.edges}
          nodeTypes={NODE_TYPES}
          colorMode="dark"
          fitView
          fitViewOptions={{ nodes: firstBlock, padding: 0.06, maxZoom: 1 }}
          panOnScroll
          minZoom={0.1}
          maxZoom={1.6}
          deleteKeyCode={null}
          onNodesChange={onNodesChange}
          onConnect={onConnect}
          onReconnect={(_, c) => interpret(c)?.()}
          onConnectStart={startWiring}
          onConnectEnd={onConnectEnd}
          onReconnectStart={startWiring}
          onReconnectEnd={stopWiring}
          isValidConnection={isValidConnection}
          onPaneClick={() => setSelected(null)}
        >
          <Background id="minor" variant={BackgroundVariant.Lines} gap={24} lineWidth={1} color="#171c24" />
          <Background
            id="major"
            variant={BackgroundVariant.Lines}
            gap={120}
            lineWidth={1}
            color="#232a35"
            bgColor="transparent"
          />
          <Controls showInteractive={false} />
          <MiniMap
            pannable
            zoomable
            maskColor="rgba(15,17,23,0.7)"
            nodeColor={(n) => MINIMAP_COLOR[(n.data as FlowData).kind] ?? "#8b949e"}
          />
        </ReactFlow>
      </div>
    </div>
  );
}

function startWiring() {
  window.getSelection()?.removeAllRanges();
  document.body.classList.add("arena-wiring");
}

function stopWiring() {
  document.body.classList.remove("arena-wiring");
}

const MINIMAP_COLOR: Record<string, string> = {
  start: "#3fb950",
  victory: "#e3b341",
  wave: "#58a6ff",
  spawner: "#f0883e",
  enemy: "#ff7b72",
  point: "#c58af9",
  message: "#39c5cf",
};
