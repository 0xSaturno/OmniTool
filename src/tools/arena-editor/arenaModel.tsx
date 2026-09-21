/** Types and helpers shared by the Arena Editor tabs and its graph views. */

export const STYLE_ORDER = ["volume", "portal", "animclue", "static"];

/**
 * Spawn points the game actually spawns enemies at. Static volumes are
 * position markers (arena centre, bounce pads, triggers): twice in a1, bots
 * bound to one still came out of the portals.
 */
export const SPAWNABLE_STYLES = ["volume", "portal", "animclue"];

export const STYLE_LABELS: Record<string, string> = {
  volume: "Spawn volumes — enemy just appears",
  portal: "Rift portals — enemy needs a portal entry animation",
  animclue: "Anim clues — enemy plays a scripted entry",
  static: "Static volumes — position markers, not spawn points; enemies fall back to portals",
};

export interface ArenaPrius {
  id: number;
  label: string;
  owners: string[];
  json: string;
  size: number;
}

export interface ArenaActorAsset {
  index: number;
  path: string;
  asset_id: string;
  instances: string[];
  instance_ids: string[];
  is_enemy: boolean;
  prius_ids: number[];
}

export interface ArenaAssetRef {
  index: number;
  path: string;
  asset_id: string;
  ext_hash: string;
}

export interface ArenaSpawnBinding {
  var: number;
  kind: "actor" | "group" | "unresolved";
  id: string;
  label: string;
  asset: string;
  style: string;
  via: string;
}

export interface ArenaSpawnTarget {
  id: string;
  kind: "actor" | "group";
  label: string;
  asset: string;
  count: number;
  style: string;
}

export interface ArenaSpawner {
  node: number;
  template: string;
  template_var: number | null;
  template_id: string | null;
  num_spawns: number | null;
  num_spawns_var: number | null;
  max_simultaneous: number | null;
  prius_id: number | null;
  groups: string[];
  locations: ArenaSpawnBinding[];
}

export type MessageWhen = "start" | "cleared";
/** One per HUD message slot (`MessageType`), plus the help box. */
export type MessageStyle =
  | "banner"
  | "help"
  | "wave"
  | "victory"
  | "generic"
  | "pickup"
  | "collectible"
  | "location"
  | "planet"
  | "corner"
  | "tutorial";

export const STYLE_TEXT: Record<MessageStyle, string> = {
  banner: "Banner",
  help: "Help box",
  wave: "Arena wave banner",
  victory: "Stock Victory! (text ignored)",
  generic: "Generic",
  pickup: "Pickup",
  collectible: "Collectible",
  location: "Location",
  planet: "Planet",
  corner: "Corner",
  tutorial: "Tutorial",
};

const STYLE_GROUPS: { label: string; styles: MessageStyle[] }[] = [
  { label: "Tested in game", styles: ["banner", "help", "wave", "victory"] },
  {
    label: "Untested",
    styles: ["generic", "pickup", "collectible", "location", "planet", "corner", "tutorial"],
  },
];

/** Centre-screen styles that collide with the stock between-wave banners. */
export const CENTRE_STYLES: MessageStyle[] = ["banner", "wave", "victory"];

export function StyleOptions() {
  return (
    <>
      {STYLE_GROUPS.map((g) => (
        <optgroup key={g.label} label={g.label}>
          {g.styles.map((s) => (
            <option key={s} value={s}>
              {STYLE_TEXT[s]}
            </option>
          ))}
        </optgroup>
      ))}
    </>
  );
}

export interface ArenaVictory {
  replaced: boolean;
  text: string;
  style: MessageStyle;
}

export interface WaveMessage {
  node: number;
  wave: number;
  when: MessageWhen;
  style: MessageStyle;
  text: string;
  duration: number;
  delay: number;
  prius_id: number | null;
}

/** A message as edited in the UI; `node` is set for ones already in the zone. */
export interface MessageDraft {
  key: string;
  node?: number;
  wave: number;
  when: MessageWhen;
  style: MessageStyle;
  text: string;
  duration: number;
  delay: number;
}

export interface ArenaText {
  key: string;
  label: string;
  var: number;
  value: string;
}

/// Seconds a start message waits so the stock "Wave N" banner has cleared.
export const START_DELAY = 3;

/** HUD text is typed on one line; `\n` stands for a line break. */
export const toGameText = (s: string) => s.replace(/\\n/g, "\n");
export const fromGameText = (s: string) => s.replace(/\n/g, "\\n");

export interface ArenaWave {
  number: number;
  signals: string[];
  spawners: ArenaSpawner[];
  total: number;
  messages: WaveMessage[];
  can_message_start: boolean;
  can_message_cleared: boolean;
  clone_warning: string | null;
}

export interface ArenaZoneData {
  zone_name: string;
  action_count: number;
  actor_count: number;
  waves: ArenaWave[];
  texts: ArenaText[];
  victory: ArenaVictory | null;
  node_type_counts: [string, number][];
  actor_groups: string[];
  script_priuses: ArenaPrius[];
  actor_priuses: ArenaPrius[];
  actor_assets: ArenaActorAsset[];
  model_names: string[];
  asset_refs: ArenaAssetRef[];
  spawn_targets: ArenaSpawnTarget[];
  clones: ArenaCloneInfo[];
}

/** Where a previewed copy's own nodes, vars and blobs begin. */
export interface ArenaCloneInfo {
  source: number;
  new_number: number;
  first_node: number;
  first_var: number;
  first_prius: number;
  warning: string | null;
}

export type Typed = { Type: string; ArrayKind?: string; Value: unknown };
export type BlobKind = "script" | "actor";

/** Quote 16+ digit integers so 64-bit ids survive `JSON.parse` for display. */
export function parseTypedSafe(text: string): Record<string, Typed> {
  try {
    return JSON.parse(text.replace(/("Value":\s*)(-?\d{16,})/g, '$1"$2"'));
  } catch {
    return {};
  }
}

export function isTyped(v: unknown): v is Typed {
  return typeof v === "object" && v !== null && "Type" in v && "Value" in v;
}

/** Pull the couple of bot fields worth showing without expanding the card. */
export function botSummary(blob: ArenaPrius): string[] {
  const json = parseTypedSafe(blob.json) as Record<string, unknown>;
  const dig = (path: string[]): unknown => {
    let cur: unknown = json;
    for (const seg of path) {
      const entry = (cur as Record<string, Typed> | undefined)?.[seg];
      if (!entry) return undefined;
      cur = entry.Value;
    }
    return cur;
  };
  const out: string[] = [];
  const health = dig(["BotBaseData", "Health"]);
  if (typeof health === "number") out.push(`${health} HP`);
  const pool = dig(["BotData", "AttackJobPool"]);
  if (typeof pool === "string") out.push(pool.replace(/^k/, ""));
  if (dig(["BotData", "OneHitDeath"]) === true) out.push("one-hit");
  return out;
}

/**
 * Every edit the graph's Easy Mode can make, backed by the same pending-edit
 * state as the Waves, Pacing and Enemies tabs — so the two stay in step and
 * saving writes one payload.
 */
export interface ArenaEditApi {
  data: ArenaZoneData;
  numSpawns(s: ArenaSpawner): number;
  setNumSpawns(s: ArenaSpawner, value: number): void;
  bindingId(b: ArenaSpawnBinding): string;
  setBinding(b: ArenaSpawnBinding, id: string): void;
  templateId(s: ArenaSpawner): string | null;
  setTemplate(s: ArenaSpawner, id: string): void;
  priusValue(kind: BlobKind, id: number, path: string[], fallback: unknown): unknown;
  setPrius(kind: BlobKind, id: number, path: string[], value: unknown): void;
  assetPath(a: ArenaActorAsset): string;
  setAssetPath(a: ArenaActorAsset, path: string): void;
  /** Pending copies, already applied to `data` by the backend preview. */
  clones: { source: number; new_number: number }[];
  previewing: boolean;
  cloneSource(wave: number): number | undefined;
  /** Why `wave` can't be duplicated, or null when it can. */
  cloneBlocker(wave: number): string | null;
  queueClone(wave: number, count: number): void;
  isNewestClone(wave: number): boolean;
  removeLastClone(): void;
  messageRows(wave: number, existing: WaveMessage[]): MessageDraft[];
  addMessage(wave: number, when: MessageWhen): void;
  updateMessage(d: MessageDraft, patch: Partial<MessageDraft>): void;
  moveMessage(d: MessageDraft, wave: number, when: MessageWhen): void;
  removeMessage(d: MessageDraft): void;
  isDirtyMessage(d: MessageDraft): boolean;
  titleText: { value: string; edited: boolean; set(v: string): void } | null;
  victory: {
    text: string;
    style: MessageStyle;
    edited: boolean;
    setText(v: string): void;
    setStyle(s: MessageStyle): void;
  } | null;
}
