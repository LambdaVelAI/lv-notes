// Links to other notes, to blocks, and to the web.
//
// A link is a MyST `link` (a mark in the editor). A link inside the project
// stores its target's stable ID as the url: `doc_…` for a note, `node_…` for
// a block. It never stores a path or a title, so it survives renames and
// moves (invariant 4), and the core's graph projection reads the ID straight
// off the url.

import { type Mark, type Node as PMNode } from "prosemirror-model";
import { type Command, type EditorState, Plugin, TextSelection } from "prosemirror-state";
import { Decoration, DecorationSet } from "prosemirror-view";
import { schema } from "./schema";

/** Transactions with this meta ask the app to open the link picker. */
export const LINK_REQUEST = "lv-link-request";

const TARGET_ID = /^(doc|node)_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/** Whether a link url names a note or block in the project. */
export const isInternalTarget = (url: unknown): url is string => typeof url === "string" && TARGET_ID.test(url);

export interface LinkRange {
  from: number;
  to: number;
  mark: Mark;
}

/** Runs of the same link mark in an inline block starting at `start`. */
function linkRuns(block: PMNode, start: number): LinkRange[] {
  const runs: LinkRange[] = [];
  block.forEach((child, offset) => {
    const mark = child.marks.find((m) => m.type === schema.marks.link);
    const from = start + offset;
    const last = runs[runs.length - 1];
    if (mark && last && last.to === from && last.mark.eq(mark)) last.to = from + child.nodeSize;
    else if (mark) runs.push({ from, to: from + child.nodeSize, mark });
  });
  return runs;
}

/**
 * The link the selection is in, with its whole extent: the cursor is inside
 * it or at its end, or the selection lies within it.
 */
export function linkAt(state: EditorState): LinkRange | null {
  const { $from, $to, empty, from, to } = state.selection;
  if (!$from.parent.inlineContent || !$from.sameParent($to)) return null;
  const runs = linkRuns($from.parent, $from.start());
  return runs.find((r) => (empty ? r.from < from && from <= r.to : r.from <= from && to <= r.to)) ?? null;
}

/**
 * Links the selection to `url`. With an empty selection it edits the link at
 * the cursor, or else inserts `text` (or the url) as a new link. A link
 * keeps its node ID when edited; new ones get theirs from the ID plugin.
 */
export function setLink(url: string, text?: string): Command {
  return (state, dispatch) => {
    if (!url) return false;
    const link = schema.marks.link;
    const { empty, from, to, $from } = state.selection;
    if (!$from.parent.inlineContent || $from.parent.type.spec.code) return false;
    if (!dispatch) return true;
    const tr = state.tr;
    const existing = empty ? linkAt(state) : null;
    if (existing) {
      tr.removeMark(existing.from, existing.to, link);
      tr.addMark(existing.from, existing.to, link.create({ ...existing.mark.attrs, url }));
    } else if (empty) {
      const label = text || url;
      const marks = link.create({ url }).addToSet(($from.marks() ?? []).filter((m) => m.type !== link));
      tr.insert(from, schema.text(label, marks));
      tr.setSelection(TextSelection.create(tr.doc, from + label.length));
    } else {
      tr.removeMark(from, to, link).addMark(from, to, link.create({ url }));
    }
    dispatch(tr.scrollIntoView());
    return true;
  };
}

/** Removes the link at the cursor, or the links in the selection. */
export const removeLink: Command = (state, dispatch) => {
  const range = linkAt(state) ?? (state.selection.empty ? null : state.selection);
  if (!range || !state.doc.rangeHasMark(range.from, range.to, schema.marks.link)) return false;
  dispatch?.(state.tr.removeMark(range.from, range.to, schema.marks.link));
  return true;
};

/** Asks the app to open the link picker (⌘K, or typing `[[`). */
export const requestLink: Command = (state, dispatch) => {
  if (!state.selection.$from.parent.inlineContent || state.selection.$from.parent.type.spec.code) return false;
  dispatch?.(state.tr.setMeta(LINK_REQUEST, true));
  return true;
};

/**
 * Where the node or link with this ID is: a block's position (`node`), or a
 * link's text range. Null if the document has neither.
 */
export function findId(doc: PMNode, id: string): { pos: number; node?: PMNode; to?: number } | null {
  let found: { pos: number; node?: PMNode; to?: number } | null = null;
  doc.descendants((node, pos) => {
    if (found) return false;
    if (node.attrs.id === id) {
      found = { pos, node };
      return false;
    }
    if (node.inlineContent) {
      const run = linkRuns(node, pos + 1).find((r) => r.mark.attrs.id === id);
      if (run) found = { pos: run.from, to: run.to };
    }
    return true;
  });
  return found;
}

/** Transactions with this meta highlight a range briefly (`null` clears it). */
export const FLASH = "lv-flash";

/** Shows a short highlight on a block or link the app revealed. */
export const flashPlugin: Plugin<DecorationSet> = new Plugin<DecorationSet>({
  state: {
    init: () => DecorationSet.empty,
    apply(tr, set) {
      const flash: { from: number; to: number; block: boolean } | null | undefined = tr.getMeta(FLASH);
      if (flash === undefined) return set.map(tr.mapping, tr.doc);
      if (!flash) return DecorationSet.empty;
      const deco = flash.block
        ? Decoration.node(flash.from, flash.to, { class: "flash" })
        : Decoration.inline(flash.from, flash.to, { class: "flash" });
      return DecorationSet.create(tr.doc, [deco]);
    },
  },
  props: { decorations: (state): DecorationSet | undefined => flashPlugin.getState(state) },
});
