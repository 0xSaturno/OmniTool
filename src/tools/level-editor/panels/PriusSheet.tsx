/**
 * THE PROPERTY SHEET.
 *
 * One placed actor carries a list of *component priuses* — `Aim`, `Health`,
 * `TriggerVolume`, `Spawner` … — and each one is a blob of typed, named
 * fields. `prius_actor_sheet` decodes them; this file is the only thing that
 * shows them.
 *
 * THREE RULES THIS PANEL EXISTS TO ENFORCE
 *
 * 1. LABEL WITH THE SCHEMA TYPE, READ WITH THE WIRE TYPE. The zone builder
 *    erases `select`, `bitfield` and `file` into a plain inline `string`, and
 *    it narrows a number into the smallest type that represents the authored
 *    value — measured over 230 428 shipped actor-prius fields, only 39.2% of
 *    the 35 900 fields the schema calls `float` are actually `f32` on the wire;
 *    20 897 are `u8`. So `view.schema_type` is the only way to know a text
 *    field is a dropdown or a path, while `view.type` is what decides how many
 *    bytes the value occupies and therefore what may be written.
 *
 * 2. A FIELD THAT CANNOT CHANGE IN PLACE SAYS SO, AND SAYS WHY. Saving is an
 *    equal-length in-place patch, so a `SpawnDelay` stored as `u8 20` can
 *    become 30 but not 20.5, and a `select` member can only be swapped for a
 *    member whose name is the same number of bytes. Every such limit is
 *    rendered next to the field, before the user types — never as a silent
 *    rejection afterwards. {@link editability} is the single place that
 *    decides.
 *
 * 3. A REFERENCE IS A LINK, NOT A NUMBER. `tuid` fields are the level's own
 *    wiring: 0 of 17 117 non-zero shipped values is a TOC asset id, 15 477 are
 *    the uid of an actor in the same zone. So a tuid resolves through the
 *    zone's own index into a clickable target and only falls back to hex.
 *
 * ROW BUDGET. A sheet is bounded but not small (the widest shipped actor runs
 * to a few hundred leaves). Groups start collapsed except the first, arrays
 * render at most {@link ARRAY_CAP} elements and each group at most
 * {@link ROW_CAP} rows, both with an explicit "+N more" line — nothing here
 * can render an unbounded list.
 */

import { useEffect, useMemo, useState } from "react";
import { LuChevronDown, LuChevronRight, LuLink, LuLock, LuRotateCcw } from "react-icons/lu";
import type {
  ActorSheetsDto,
  PriusEditPlan,
  PriusFieldView,
  PriusSheetDto,
  StagedPriusEdit,
  TuidTarget,
} from "../types";
import styles from "./PriusSheet.module.css";

/** Elements of one array field rendered before the panel gives up. */
export const ARRAY_CAP = 64;
/** Leaf rows rendered inside one component group before the panel gives up. */
export const ROW_CAP = 240;

export interface PlanEditArgs {
  row: number;
  scriptAction: boolean;
  fieldPath: string;
  number?: number;
  text?: string;
}

export interface PriusSheetProps {
  /** `prius_actor_sheet`, or null while it has not been asked for. */
  sheets: ActorSheetsDto | null;
  /** A single script-action prius, as an alternative to `sheets`. */
  scriptSheet?: PriusSheetDto | null;
  zonePath: string | null;
  loading?: boolean;
  error?: string | null;
  /** `tuid -> target` for this zone; null while it is still being built. */
  tuids?: Map<string, TuidTarget> | null;
  /** Ask the backend whether an edit is legal and for its bytes. */
  onPlanEdit?: (args: PlanEditArgs) => Promise<PriusEditPlan>;
  staged?: StagedPriusEdit[];
  onUnstage?: (e: StagedPriusEdit) => void;
  onSelectTuid?: (target: TuidTarget) => void;
}

/* ------------------------------------------------------------------ */
/* Type and editability                                                */
/* ------------------------------------------------------------------ */

/**
 * What to *call* the field. The schema type wins because the authoring tool erased
 * the interesting ones; the wire type is shown beside it when they differ so
 * the byte width is never hidden.
 */
export function displayType(f: PriusFieldView): string {
  return f.schema_type ?? f.type;
}

export interface Editability {
  /** How to render the control. */
  mode: "number" | "bool" | "select" | "text" | "readonly";
  /** Present when `mode === "readonly"`: shown verbatim next to the field. */
  reason?: string;
  /** Extra constraint text shown under an editable control. */
  hint?: string;
  /**
   * The long form of {@link hint}, on hover. A wide sheet repeats one
   * constraint on every narrowed field — eight identical 20-word sentences in
   * one shipped actor — so the row carries the short rule and the reasoning
   * waits in the tooltip.
   */
  hintTitle?: string;
  /** For `select`: the members that fit the slot, and those that do not. */
  fits?: string[];
  tooLong?: string[];
}

/** Bytes one string occupies on the wire. The blob stores UTF-8. */
export function byteLength(s: string): number {
  if (typeof TextEncoder !== "undefined") return new TextEncoder().encode(s).length;
  return unescape(encodeURIComponent(s)).length;
}

/**
 * Bytes one inline string OCCUPIES, which is not its length.
 *
 * `core::prius::encode_value` writes a `u32` length, a `u32` crc32, a `u64`
 * crc64, the UTF-8 bytes and a NUL, then pads the whole thing to a multiple of
 * four. So the slot is `16 + ceil4(len + 1)` and a value may grow or shrink
 * freely inside its own four-byte bucket — `kNeutral` (8) and `kBlueTeam` (9)
 * both live in a 28-byte slot.
 *
 * MEASURED, not assumed: asked `prius_plan_edit` about every enum member of
 * every select in five shipped zones — 1 722 probes — this rule predicts the
 * backend's answer 1 722 times out of 1 722. The "same number of bytes" rule
 * this panel used before is wrong on 623 of those 1 722 (36.2%), and wrong only
 * in the direction that HIDES a legal edit.
 */
export function slotBytes(s: string): number {
  return 16 + Math.ceil((byteLength(s) + 1) / 4) * 4;
}

/**
 * The inclusive length range that fits a `slot`-byte string field, as
 * `[min, max]`. Inverse of {@link slotBytes}: a 28-byte slot takes 8…11 bytes,
 * a 20-byte slot takes 0…3.
 */
export function slotRange(slot: number): [number, number] {
  const text = slot - 16; // the padded `len + 1` region
  return [Math.max(0, text - 4), Math.max(0, text - 1)];
}

/**
 * The slot a string field actually has. `width` is the span the decoder
 * measured, which is authoritative; the current value's own encoding is the
 * fallback for a field that arrived without one.
 */
function fieldSlot(f: PriusFieldView): number {
  if (typeof f.width === "number" && f.width >= 20) return f.width;
  return slotBytes(String(f.value ?? f.text ?? ""));
}

/**
 * The single place that decides what a field offers, and what it says when it
 * offers nothing. Mirrors `core::prius::Edit` plus the narrowing measurement;
 * `prius_plan_edit` enforces the same rules again server-side, so a bug here
 * can only ever be over-restrictive, never a bad write.
 */
export function editability(f: PriusFieldView): Editability {
  // An array or a struct has no single slot to overwrite.
  if (f.count > 1) {
    return {
      mode: "readonly",
      reason: `array of ${f.count} — edit an element's own row, not the array`,
    };
  }
  switch (f.edit) {
    case "not_editable":
      // An empty array is one byte on the wire with nothing in it. Without
      // this the row read "[1 values]" — the single `EmptyArray` marker — and
      // said "no value", which is two wrong answers to the same question.
      if (f.type === "empty-array") {
        return {
          mode: "readonly",
          reason:
            "an empty array: the blob spends a single marker byte on it, so there is no element to overwrite in place",
        };
      }
      return {
        mode: "readonly",
        reason: f.edit_note ?? (f.type === "struct" ? "a struct — edit its fields" : "no value"),
      };
    case "reference":
      // The backend's note covers all three reference shapes at once ("a
      // 64-bit asset/tuid reference…"), which is wrong for a tuid in the only
      // way that matters: measured over the shipped build, 0 of 17 117
      // non-zero tuids is a TOC asset id — they name objects in this very
      // zone. So a tuid gets the precise sentence and the generic note stays
      // in the tooltip.
      if (f.type === "tuid") {
        return {
          mode: "readonly",
          reason:
            "a tuid: the uid of another object in this zone. Retarget it by picking an object, not by typing a number",
        };
      }
      return {
        mode: "readonly",
        reason:
          f.edit_note ??
          (f.schema_type === "file"
            ? "an asset path. The zone builder derives the zone's backing asset list from these, so repointing one can make the zone load without the asset"
            : "an engine-allocated id; cloning one desyncs whatever allocated it"),
      };
    case "select_same_len": {
      const cur = String(f.value ?? f.text);
      const slot = fieldSlot(f);
      const [lo, hi] = slotRange(slot);
      const opts = f.options ?? [];
      const fits = opts.filter((o) => slotBytes(o) === slot);
      const tooLong = opts.filter((o) => slotBytes(o) !== slot);
      if (opts.length === 0) {
        return {
          mode: "readonly",
          reason:
            "a select, but no type database is loaded, so its members are unknown — load rcra_ddl.json and this becomes a dropdown",
        };
      }
      if (fits.length <= 1) {
        return {
          mode: "readonly",
          reason: `a select stored as an inline string in a ${slot}-byte slot, which holds a name of ${lo}–${hi} bytes; of its ${opts.length} members only "${cur}" is that size`,
        };
      }
      return {
        mode: "select",
        fits,
        tooLong,
        hint:
          tooLong.length > 0
            ? `${fits.length} of ${opts.length} members fit the ${slot}-byte slot (names of ${lo}–${hi} bytes); the rest would change the blob size`
            : undefined,
      };
    }
    case "string_same_len": {
      const slot = fieldSlot(f);
      const [lo, hi] = slotRange(slot);
      return {
        mode: "text",
        hint: `inline string in a ${slot}-byte slot: ${lo}–${hi} bytes of text (16 bytes of header, then the text, a NUL and padding to 4)`,
      };
    }
    case "scalar": {
      if (f.type === "bool") return { mode: "bool" };
      const range =
        f.min !== undefined && f.max !== undefined
          ? `${fmtBound(f.min)} … ${fmtBound(f.max)}`
          : null;
      // The narrowing constraint, stated before the user types it.
      if (f.integral) {
        const narrowed = f.schema_type === "f32" || f.schema_type === "f64";
        return {
          mode: "number",
          hint: narrowed
            ? `stored as ${f.type}, not the ${f.schema_type} the schema declares: whole numbers only${range ? `, ${range}` : ""}`
            : `${f.type}: whole numbers only${range ? `, ${range}` : ""}`,
          hintTitle: narrowed
            ? `The zone builder stores a field in the narrowest type that represents the authored value, so this ${f.schema_type} is ${f.type} on the wire. Saving rewrites the same bytes in place, so a fractional value cannot fit: it would need a wider slot than this blob has.`
            : undefined,
        };
      }
      return { mode: "number", hint: range ? `${f.type}, ${range}` : f.type };
    }
    default:
      return { mode: "readonly", reason: "unknown edit class" };
  }
}

/**
 * The backend's refusal, minus the plumbing.
 *
 * `prius_plan_edit` answers through `ToolkitError`, so a perfectly clear
 * sentence arrives as `Parse error: prius: FacingData.Type needs 48 bytes but
 * the slot is 44 …`. The two prefixes name the error type and the module, not
 * anything the person who typed the value can act on. The rest is shown
 * verbatim — never reworded, never swallowed.
 */
export function readableError(e: string | null | undefined): string {
  if (!e) return "the backend refused this value without saying why";
  return e.replace(/^(parse|io|format)\s+error:\s*/i, "").replace(/^prius:\s*/i, "");
}

function fmtBound(n: number): string {
  if (!Number.isFinite(n)) return String(n);
  if (Math.abs(n) >= 1e15) return n.toExponential(3);
  return String(n);
}

/** A u64 that arrived as a `0x…` string or a decimal string, as decimal. */
function tuidKey(v: unknown): string | null {
  if (typeof v === "number") return Number.isFinite(v) ? String(Math.trunc(v)) : null;
  if (typeof v !== "string" || v.length === 0) return null;
  try {
    const n = BigInt(v);
    return (n < 0n ? n + (1n << 64n) : n).toString();
  } catch {
    return null;
  }
}

/* ------------------------------------------------------------------ */
/* Leaf value rendering                                                */
/* ------------------------------------------------------------------ */

function AssetPath({ path }: { path: string }) {
  const file = path.split(/[\\/]/).pop() ?? path;
  return (
    <span className={styles.assetPath} title={path}>
      <span className={styles.assetDir}>{path.slice(0, path.length - file.length)}</span>
      <span className={styles.assetFile}>{file}</span>
    </span>
  );
}

function TuidValue({
  raw,
  tuids,
  onSelect,
}: {
  raw: unknown;
  tuids?: Map<string, TuidTarget> | null;
  onSelect?: (t: TuidTarget) => void;
}) {
  const key = tuidKey(raw);
  const hex = typeof raw === "string" ? raw : String(raw);
  if (key === null || key === "0") return <span className={styles.nullRef}>none</span>;
  const target = tuids?.get(key);
  if (!target) {
    return (
      <span className={styles.unresolved} title={`tuid ${key} — nothing in this zone carries it`}>
        {hex}
        <span className={styles.dim}> (not in this zone)</span>
      </span>
    );
  }
  const label = target.name || `${target.kind} ${target.actorIndex ?? target.offset}`;
  return (
    <button
      type="button"
      className={styles.tuidLink}
      title={`${target.kind} — ${hex}`}
      onClick={() => onSelect?.(target)}
      disabled={!onSelect}
    >
      <LuLink />
      {label}
      <span className={styles.refKind}>{target.kindName ?? target.kind}</span>
    </button>
  );
}

/** A value with no typed control: printed, not edited. */
function PlainValue({
  f,
  tuids,
  onSelectTuid,
}: {
  f: PriusFieldView;
  tuids?: Map<string, TuidTarget> | null;
  onSelectTuid?: (t: TuidTarget) => void;
}) {
  const shown = displayType(f);
  if (f.type === "empty-array") {
    return <span className={`${styles.plainValue} ${styles.dim}`}>empty array</span>;
  }
  if (f.type === "tuid") return <TuidValue raw={f.value} tuids={tuids} onSelect={onSelectTuid} />;
  if (f.type === "asset-id") {
    return <span className={styles.mono}>{String(f.value)}</span>;
  }
  if (shown === "file" && typeof f.value === "string" && f.value.length > 0) {
    return <AssetPath path={f.value} />;
  }
  if (typeof f.value === "boolean") {
    return <span className={f.value ? styles.boolOn : styles.boolOff}>{String(f.value)}</span>;
  }
  if (f.value === null || f.value === undefined) return <span className={styles.dim}>—</span>;
  if (typeof f.value === "string" && f.value.length === 0) {
    return <span className={styles.dim}>(empty)</span>;
  }
  return <span className={styles.plainValue}>{f.text || String(f.value)}</span>;
}

/* ------------------------------------------------------------------ */
/* One editable leaf                                                   */
/* ------------------------------------------------------------------ */

interface LeafProps {
  field: PriusFieldView;
  path: string;
  row: number;
  scriptAction: boolean;
  depth: number;
  tuids?: Map<string, TuidTarget> | null;
  staged?: StagedPriusEdit;
  onPlanEdit?: (args: PlanEditArgs) => Promise<PriusEditPlan>;
  onUnstage?: () => void;
  onSelectTuid?: (t: TuidTarget) => void;
}

function Leaf({
  field,
  path,
  row,
  scriptAction,
  depth,
  tuids,
  staged,
  onPlanEdit,
  onUnstage,
  onSelectTuid,
}: LeafProps) {
  const cap = useMemo(() => editability(field), [field]);
  const shipped = field.text;
  const [draft, setDraft] = useState<string>(staged ? staged.newText : shipped);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Re-seed when the selection moves to a different actor or the edit is
  // reverted somewhere else. Keyed on the value the backend last reported.
  useEffect(() => {
    setDraft(staged ? staged.newText : shipped);
    setRefusal(null);
  }, [shipped, staged]);

  const editable = cap.mode !== "readonly" && !!onPlanEdit;

  const commit = async (value: string | number | boolean) => {
    if (!onPlanEdit) return;
    setBusy(true);
    setRefusal(null);
    try {
      const args: PlanEditArgs =
        typeof value === "string"
          ? { row, scriptAction, fieldPath: path, text: value }
          : { row, scriptAction, fieldPath: path, number: Number(value) };
      const plan = await onPlanEdit(args);
      if (!plan.ok) setRefusal(readableError(plan.error));
    } catch (e) {
      setRefusal(String(e));
    } finally {
      setBusy(false);
    }
  };

  const commitNumber = (text: string) => {
    const n = Number(text);
    if (text.trim() === "" || !Number.isFinite(n)) {
      setRefusal(`"${text}" is not a number`);
      return;
    }
    // Pre-refuse with the real reason rather than letting the slot width come
    // back as a server error the user cannot act on.
    if (field.integral && !Number.isInteger(n)) {
      setRefusal(
        `this value is stored as ${field.type}, one${field.width === 1 ? " byte" : ` ${field.width} bytes`} wide — ${n} needs a fractional slot, and saving only ever rewrites the same bytes in place`,
      );
      return;
    }
    if (field.min !== undefined && n < field.min) {
      setRefusal(`below the ${field.type} minimum ${fmtBound(field.min)}`);
      return;
    }
    if (field.max !== undefined && n > field.max) {
      setRefusal(`above the ${field.type} maximum ${fmtBound(field.max)}`);
      return;
    }
    void commit(n);
  };

  const label = field.label && field.label !== field.name ? field.label : field.name;
  const dirty = !!staged;

  return (
    <div
      className={`${styles.leaf} ${dirty ? styles.leafDirty : ""}`}
      style={{ paddingLeft: `${depth * 0.75}rem` }}
    >
      <div className={styles.leafHead}>
        <span className={styles.fieldName} title={`${field.name}  ${field.hash}  (${field.name_source})`}>
          {label}
          {field.name_source === "hash" && <span className={styles.unnamed} title="no name in the zone's string pool and none in the type database — this is crc32(name)">?</span>}
        </span>
        <span className={styles.typeTag} title={typeTitle(field)}>
          {displayType(field)}
          {field.schema_type && field.schema_type !== field.type && (
            <span className={styles.wireType}>·{field.type}</span>
          )}
        </span>
      </div>

      <div className={styles.leafBody}>
        {cap.mode === "readonly" && (
          <>
            <PlainValue f={field} tuids={tuids} onSelectTuid={onSelectTuid} />
            <span className={styles.lockNote} title={cap.reason}>
              <LuLock />
              {cap.reason}
            </span>
          </>
        )}

        {cap.mode === "bool" && (
          <label className={styles.checkRow}>
            <input
              type="checkbox"
              disabled={!editable || busy}
              checked={draft === "true"}
              onChange={(e) => {
                setDraft(String(e.target.checked));
                void commit(e.target.checked);
              }}
            />
            <span>{draft}</span>
          </label>
        )}

        {cap.mode === "number" && (
          <input
            type="text"
            inputMode="decimal"
            className={`${styles.input} ${styles.numeric}`}
            disabled={!editable || busy}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => draft !== (staged ? staged.newText : shipped) && commitNumber(draft)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitNumber(draft);
              if (e.key === "Escape") {
                setDraft(staged ? staged.newText : shipped);
                setRefusal(null);
              }
            }}
          />
        )}

        {cap.mode === "select" && (
          <select
            className={styles.input}
            disabled={!editable || busy}
            value={draft}
            onChange={(e) => {
              setDraft(e.target.value);
              void commit(e.target.value);
            }}
          >
            {!(cap.fits ?? []).includes(draft) && <option value={draft}>{draft}</option>}
            {(cap.fits ?? []).map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        )}

        {cap.mode === "text" && (
          <input
            type="text"
            className={styles.input}
            disabled={!editable || busy}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => draft !== (staged ? staged.newText : shipped) && void commit(draft)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void commit(draft);
              if (e.key === "Escape") {
                setDraft(staged ? staged.newText : shipped);
                setRefusal(null);
              }
            }}
          />
        )}

        {staged && (
          <button
            type="button"
            className={styles.revertField}
            title={`was "${staged.oldText}"`}
            onClick={() => onUnstage?.()}
          >
            <LuRotateCcw />
          </button>
        )}
      </div>

      {cap.hint && !refusal && (
        <div className={styles.hint} title={cap.hintTitle ?? cap.hint}>
          {cap.hint}
        </div>
      )}
      {refusal && <div className={styles.refusal}>{refusal}</div>}
      {staged && (
        <div className={styles.dirtyNote}>
          staged: <span className={styles.wasValue}>{staged.oldText}</span> →{" "}
          <span className={styles.nowValue}>{staged.newText}</span>{" "}
          <span className={styles.dim}>
            ({staged.bytes.length} B at {staged.sectionTag}+0x
            {staged.sectionOffset.toString(16)})
          </span>
        </div>
      )}
    </div>
  );
}

function typeTitle(f: PriusFieldView): string {
  const parts = [`wire type ${f.type}`];
  if (f.schema_type && f.schema_type !== f.type) parts.push(`schema type ${f.schema_type}`);
  if (f.width !== null) parts.push(`${f.width} byte${f.width === 1 ? "" : "s"}`);
  if (f.offset !== null) parts.push(`at +0x${f.offset.toString(16)} in the blob`);
  // A select's schema default is the MEMBER INDEX, not the member name, so
  // printing it raw claims `MotionStyle` defaults to `0`.
  if (f.default !== undefined) {
    const opts = f.options;
    const named =
      opts && typeof f.default === "number" && f.default >= 0 && f.default < opts.length
        ? `${opts[f.default]} (member ${f.default})`
        : JSON.stringify(f.default);
    parts.push(`schema default ${named}`);
  }
  parts.push(`name from ${f.name_source}`);
  return parts.join(" · ");
}

/* ------------------------------------------------------------------ */
/* Structs and arrays                                                  */
/* ------------------------------------------------------------------ */

/** A JSON value with no typed view behind it (array elements past the first). */
function JsonNode({ value, depth }: { value: unknown; depth: number }) {
  if (value === null || value === undefined) return <span className={styles.dim}>—</span>;
  if (Array.isArray(value)) {
    const shown = value.slice(0, ARRAY_CAP);
    return (
      <div className={styles.jsonBlock}>
        {shown.map((v, i) => (
          <div key={i} className={styles.jsonRow} style={{ paddingLeft: `${depth * 0.6}rem` }}>
            <span className={styles.arrayIndex}>[{i}]</span>
            <JsonNode value={v} depth={depth + 1} />
          </div>
        ))}
        {value.length > shown.length && (
          <div className={styles.more}>+{value.length - shown.length} more elements not shown</div>
        )}
      </div>
    );
  }
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>);
    return (
      <div className={styles.jsonBlock}>
        {entries.map(([k, v]) => (
          <div key={k} className={styles.jsonRow} style={{ paddingLeft: `${depth * 0.6}rem` }}>
            <span className={styles.jsonKey}>{k}</span>
            <JsonNode value={v} depth={depth + 1} />
          </div>
        ))}
      </div>
    );
  }
  return <span className={styles.plainValue}>{String(value)}</span>;
}

interface NodeProps extends Omit<LeafProps, "field" | "path" | "onUnstage" | "staged"> {
  field: PriusFieldView;
  path: string;
  stagedBy: Map<string, StagedPriusEdit>;
  onUnstageEdit?: (e: StagedPriusEdit) => void;
}

/**
 * Renders a list of sibling fields, at most {@link ROW_CAP} of them.
 *
 * The cap has to be applied where the ELEMENTS are built, not inside the child
 * — a component's body runs after its parent has returned, so a budget
 * decremented in the child is always still full when the parent reads it.
 */
function FieldList({
  fields,
  parentPath,
  props,
  depth,
}: {
  fields: PriusFieldView[];
  parentPath: string;
  props: Omit<NodeProps, "field" | "path" | "depth">;
  depth: number;
}) {
  const shown = fields.slice(0, ROW_CAP);
  return (
    <>
      {shown.map((c) => {
        const path = parentPath ? `${parentPath}.${c.name}` : c.name;
        return <FieldNode {...props} key={path + c.hash} field={c} path={path} depth={depth} />;
      })}
      {fields.length > shown.length && (
        <div className={styles.more} style={{ paddingLeft: `${depth * 0.75}rem` }}>
          +{fields.length - shown.length} more field
          {fields.length - shown.length === 1 ? "" : "s"} not shown (cap {ROW_CAP} rows per level)
        </div>
      )}
    </>
  );
}

function FieldNode(props: NodeProps) {
  const { field, path, depth, stagedBy, onUnstageEdit } = props;
  const [open, setOpen] = useState(depth < 1);

  const isStruct = field.type === "struct" && field.children.length > 0;
  const isArray = field.count > 1;

  if (isStruct && !isArray) {
    return (
      <div className={styles.struct}>
        <button
          type="button"
          className={styles.structHead}
          style={{ paddingLeft: `${depth * 0.75}rem` }}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? <LuChevronDown /> : <LuChevronRight />}
          <span className={styles.fieldName}>{field.label ?? field.name}</span>
          <span className={styles.typeTag}>struct · {field.children.length}</span>
        </button>
        {open && (
          <FieldList fields={field.children} parentPath={path} props={props} depth={depth + 1} />
        )}
      </div>
    );
  }

  if (isArray) {
    const arr = Array.isArray(field.value) ? field.value : [];
    return (
      <div className={styles.struct}>
        <button
          type="button"
          className={styles.structHead}
          style={{ paddingLeft: `${depth * 0.75}rem` }}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? <LuChevronDown /> : <LuChevronRight />}
          <span className={styles.fieldName}>{field.label ?? field.name}</span>
          <span className={styles.typeTag}>
            {displayType(field)}[{field.count}]
          </span>
        </button>
        {open && <JsonNode value={arr} depth={depth + 1} />}
      </div>
    );
  }

  const staged = stagedBy.get(path);
  return (
    <Leaf
      field={field}
      path={path}
      row={props.row}
      scriptAction={props.scriptAction}
      depth={depth}
      tuids={props.tuids}
      staged={staged}
      onPlanEdit={props.onPlanEdit}
      onUnstage={staged && onUnstageEdit ? () => onUnstageEdit(staged) : undefined}
      onSelectTuid={props.onSelectTuid}
    />
  );
}

/* ------------------------------------------------------------------ */
/* One component prius                                                 */
/* ------------------------------------------------------------------ */

function ComponentGroup({
  sheet,
  scriptAction,
  defaultOpen,
  tuids,
  stagedBy,
  onPlanEdit,
  onUnstageEdit,
  onSelectTuid,
}: {
  sheet: PriusSheetDto;
  scriptAction: boolean;
  defaultOpen: boolean;
  tuids?: Map<string, TuidTarget> | null;
  stagedBy: Map<string, StagedPriusEdit>;
  onPlanEdit?: (a: PlanEditArgs) => Promise<PriusEditPlan>;
  onUnstageEdit?: (e: StagedPriusEdit) => void;
  onSelectTuid?: (t: TuidTarget) => void;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const v = sheet.view;
  const dirtyCount = useMemo(() => {
    let n = 0;
    for (const e of stagedBy.values()) if (e.row === sheet.row && e.scriptAction === scriptAction) n++;
    return n;
  }, [stagedBy, sheet.row, scriptAction]);

  const childProps = {
    row: sheet.row,
    scriptAction,
    tuids,
    stagedBy,
    onPlanEdit,
    onUnstageEdit,
    onSelectTuid,
  };

  return (
    <div className={styles.group}>
      <button type="button" className={styles.groupHead} onClick={() => setOpen((o) => !o)}>
        {open ? <LuChevronDown /> : <LuChevronRight />}
        <span className={styles.component}>{v.component}</span>
        {v.type_name && v.type_name !== v.component && (
          <span className={styles.structName}>{v.type_name}</span>
        )}
        <span className={styles.counts} title={`${v.named_fields} of ${v.total_fields} fields carry a real name`}>
          {v.named_fields}/{v.total_fields}
        </span>
        {dirtyCount > 0 && <span className={styles.dirtyBadge}>{dirtyCount}</span>}
        <span className={styles.bytes}>{v.size} B</span>
      </button>
      {open && (
        <div className={styles.groupBody}>
          {v.fields.length === 0 && (
            <div className={styles.empty}>no fields — every value is the schema default</div>
          )}
          <FieldList fields={v.fields} parentPath="" props={childProps} depth={0} />
        </div>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* The panel                                                           */
/* ------------------------------------------------------------------ */

export default function PriusSheet({
  sheets,
  scriptSheet,
  zonePath,
  loading,
  error,
  tuids,
  onPlanEdit,
  staged,
  onUnstage,
  onSelectTuid,
}: PriusSheetProps) {
  const scriptAction = !!scriptSheet && !sheets;
  const list: PriusSheetDto[] = sheets ? sheets.sheets : scriptSheet ? [scriptSheet] : [];

  /** `fieldPath -> staged edit`, restricted to the rows on screen. */
  const stagedBy = useMemo(() => {
    const m = new Map<string, StagedPriusEdit>();
    for (const e of staged ?? []) {
      if (zonePath && e.zonePath !== zonePath) continue;
      if (e.scriptAction !== scriptAction) continue;
      m.set(`${e.row}|${e.fieldPath}`, e);
    }
    // The tree keys by field path alone, so re-key per row at render time.
    return m;
  }, [staged, zonePath, scriptAction]);

  if (loading) return <div className={styles.status}>decoding property sheets…</div>;
  if (error) return <div className={styles.statusError}>{error}</div>;
  if (!sheets && !scriptSheet) return null;

  if (list.length === 0) {
    return (
      <div className={styles.status}>
        this actor carries no component priuses — every value is its asset&apos;s default
      </div>
    );
  }

  return (
    <div className={styles.sheet}>
      {sheets && sheets.failures.length > 0 && (
        <div className={styles.failures}>
          {sheets.failures.map((f, i) => (
            <div key={i}>{f}</div>
          ))}
        </div>
      )}
      {list.map((s, i) => (
        <ComponentGroup
          key={`${s.row}-${s.view.component}`}
          sheet={s}
          scriptAction={scriptAction}
          // The first group is open so the panel never opens on a wall of
          // collapsed headers; the rest wait to be asked for.
          defaultOpen={i === 0 || list.length <= 2}
          tuids={tuids}
          stagedBy={rowScoped(stagedBy, s.row)}
          onPlanEdit={onPlanEdit}
          onUnstageEdit={onUnstage}
          onSelectTuid={onSelectTuid}
        />
      ))}
    </div>
  );
}

/** `"<row>|<path>"` keys narrowed to one row, keyed by path alone. */
function rowScoped(all: Map<string, StagedPriusEdit>, row: number): Map<string, StagedPriusEdit> {
  const out = new Map<string, StagedPriusEdit>();
  for (const [k, v] of all) {
    if (v.row !== row) continue;
    out.set(k.slice(k.indexOf("|") + 1), v);
  }
  return out;
}
