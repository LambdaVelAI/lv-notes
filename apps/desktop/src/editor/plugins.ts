// Editor behaviour: node IDs, keys and input rules.

import { baseKeymap, chainCommands, setBlockType, toggleMark, wrapIn, exitCode } from "prosemirror-commands";
import { history, redo, undo } from "prosemirror-history";
import { InputRule, inputRules, textblockTypeInputRule, wrappingInputRule } from "prosemirror-inputrules";
import { keymap } from "prosemirror-keymap";
import { type Node as PMNode } from "prosemirror-model";
import { liftListItem, sinkListItem, splitListItem } from "prosemirror-schema-list";
import { type Command, type EditorState, Plugin, Selection, TextSelection, type Transaction } from "prosemirror-state";
import { newNodeId } from "./ids";
import { claimMystIds, mapMystIds, mystIds } from "./myst";
import { schema } from "./schema";

/** Transactions with this meta are bookkeeping, not user edits. */
export const SYSTEM = "lv-system";

/**
 * Gives every node that needs one a unique ID: new nodes have none, and
 * splitting, pasting or duplicating copies an existing one. The first
 * occurrence (in document order) keeps it. Runs after each transaction, so
 * the core never sees a tree without IDs.
 */
export function assignIds(state: EditorState): Transaction | null {
  const tr = state.tr;
  const seen = new Set<string>();
  const claim = (id: string | null) => {
    if (id && !seen.has(id)) {
      seen.add(id);
      return id;
    }
    return newNodeId();
  };

  const docId = claim(state.doc.attrs.id);
  if (docId !== state.doc.attrs.id) tr.setDocAttribute("id", docId);

  state.doc.descendants((node, pos, parent, index) => {
    const { paragraph, opaque, opaqueInline } = schema.nodes;
    if (node.type === opaque || node.type === opaqueInline) {
      const json = claimMystIds(node.attrs.json, claim);
      if (JSON.stringify(json) !== JSON.stringify(node.attrs.json)) tr.setNodeMarkup(pos, null, { json });
      return false;
    }
    if (node.type === paragraph && node.attrs.implicit) {
      // Only the leading text of a container is implicit; a paragraph split
      // off it is a real one.
      const previous = index > 0 ? parent?.child(index - 1) : null;
      if (previous?.type === paragraph) tr.setNodeMarkup(pos, null, { ...node.attrs, implicit: false, id: newNodeId() });
    } else if ("id" in node.attrs) {
      const id = claim(node.attrs.id);
      if (id !== node.attrs.id) tr.setNodeMarkup(pos, null, { ...node.attrs, id });
    }
    if (node.inlineContent) linkIds(node, pos + 1, tr, claim);
    return true;
  });
  return tr.docChanged ? tr.setMeta(SYSTEM, true).setMeta("addToHistory", false) : null;
}

/** Link marks are MyST `link` nodes: one ID per contiguous run. */
function linkIds(block: PMNode, start: number, tr: Transaction, claim: (id: string | null) => string) {
  const link = schema.marks.link;
  let run: { from: number; to: number; mark: (typeof block.marks)[number] } | null = null;
  const close = () => {
    if (!run) return;
    const id = claim(run.mark.attrs.id);
    if (id !== run.mark.attrs.id) {
      tr.removeMark(run.from, run.to, run.mark);
      tr.addMark(run.from, run.to, link.create({ ...run.mark.attrs, id }));
    }
    run = null;
  };
  block.forEach((child, offset) => {
    const mark = child.marks.find((m) => m.type === link);
    const from = start + offset;
    if (run && mark && run.mark.eq(mark)) run.to = from + child.nodeSize;
    else {
      close();
      if (mark) run = { from, to: from + child.nodeSize, mark };
    }
  });
  close();
}

export const idPlugin = new Plugin({
  appendTransaction: (trs, _old, state) => (trs.some((tr) => tr.docChanged) ? assignIds(state) : null),
});

/** Renames node IDs the core could not reuse (`[old, new]` pairs). */
export function renameIds(state: EditorState, renamed: [string, string][]): Transaction | null {
  if (!renamed.length) return null;
  const map = new Map(renamed);
  const rename = (id: string) => map.get(id) ?? id;
  const tr = state.tr;
  if (map.has(state.doc.attrs.id)) tr.setDocAttribute("id", rename(state.doc.attrs.id));
  state.doc.descendants((node, pos) => {
    if (node.type === schema.nodes.opaque || node.type === schema.nodes.opaqueInline) {
      if (mystIds(node.attrs.json).some((id) => map.has(id))) {
        tr.setNodeMarkup(pos, null, { json: mapMystIds(node.attrs.json, rename) });
      }
      return false;
    }
    if (map.has(node.attrs.id)) tr.setNodeMarkup(pos, null, { ...node.attrs, id: rename(node.attrs.id) });
    for (const mark of node.marks) {
      if (mark.type === schema.marks.link && map.has(mark.attrs.id)) {
        tr.removeMark(pos, pos + node.nodeSize, mark);
        tr.addMark(pos, pos + node.nodeSize, mark.type.create({ ...mark.attrs, id: rename(mark.attrs.id) }));
      }
    }
    return true;
  });
  return tr.docChanged ? tr.setMeta(SYSTEM, true).setMeta("addToHistory", false) : null;
}

// ---- Commands ---------------------------------------------------------------

const n = schema.nodes;
const m = schema.marks;

export const commands = {
  paragraph: setBlockType(n.paragraph),
  heading: (depth: number) => setBlockType(n.heading, { depth }),
  code: setBlockType(n.code),
  quote: wrapIn(n.blockquote),
  bullets: wrapIn(n.list, { ordered: null }),
  numbers: wrapIn(n.list, { ordered: true }),
  strong: toggleMark(m.strong),
  emphasis: toggleMark(m.emphasis),
  inlineCode: toggleMark(m.inlineCode),
  undo,
  redo,
};

/**
 * Leaves a code or math block for a new paragraph after it:
 * - `emptyLine`: the cursor is on an empty last line, which is dropped
 *   (Enter twice leaves);
 * - `lastLine` / `atEnd`: the cursor is on the last line / at the end, and
 *   nothing follows to move to (arrow keys at the end of a note);
 * - `anywhere`.
 */
export function leaveCode(how: "emptyLine" | "lastLine" | "atEnd" | "anywhere"): Command {
  return (state, dispatch, view) => {
    const { $head, empty } = state.selection;
    const block = $head.parent;
    if (!empty || !block.type.spec.code) return false;
    const atEnd = $head.parentOffset === block.content.size;
    if (how === "emptyLine" && !(atEnd && block.textContent.endsWith("\n"))) return false;
    if (how === "lastLine" || how === "atEnd") {
      const nothingAfter = !Selection.findFrom(state.doc.resolve($head.after()), 1, true);
      const onLastLine = atEnd || (how === "lastLine" && !!view?.endOfTextblock("down"));
      if (!nothingAfter || !onLastLine) return false;
    }
    if (dispatch) {
      const tr = state.tr;
      if (how === "emptyLine") tr.delete($head.pos - 1, $head.pos);
      const after = tr.mapping.map($head.after());
      tr.insert(after, n.paragraph.create());
      dispatch(tr.setSelection(TextSelection.create(tr.doc, after + 1)).scrollIntoView());
    }
    return true;
  };
}

const insertBreak: Command = (state, dispatch) => {
  dispatch?.(state.tr.replaceSelectionWith(n.break.create()).scrollIntoView());
  return true;
};

function rules() {
  return inputRules({
    rules: [
      textblockTypeInputRule(/^(#{1,6})\s$/, n.heading, (match) => ({ depth: match[1].length })),
      textblockTypeInputRule(/^```$/, n.code),
      wrappingInputRule(/^\s*>\s$/, n.blockquote),
      wrappingInputRule(/^\s*([-+*])\s$/, n.list, { ordered: null }),
      wrappingInputRule(
        /^(\d+)\.\s$/,
        n.list,
        (match) => ({ ordered: true, start: Number(match[1]) === 1 ? null : Number(match[1]) }),
        (match, node) => node.childCount + (node.attrs.start ?? 1) === Number(match[1]),
      ),
      new InputRule(/^---$/, (state, _match, start, end) => {
        const $start = state.doc.resolve(start);
        if ($start.parent.type !== n.paragraph) return null;
        return state.tr.replaceRangeWith($start.before(), end + 1, n.thematicBreak.create());
      }),
    ],
  });
}

export function plugins(): Plugin[] {
  return [
    rules(),
    keymap({
      "Mod-z": undo,
      "Shift-Mod-z": redo,
      "Mod-y": redo,
      "Mod-b": commands.strong,
      "Mod-i": commands.emphasis,
      "Mod-e": commands.inlineCode,
      "Mod-Alt-0": commands.paragraph,
      "Mod-Alt-1": commands.heading(1),
      "Mod-Alt-2": commands.heading(2),
      "Mod-Alt-3": commands.heading(3),
      "Shift-Enter": chainCommands(exitCode, insertBreak),
      "Mod-Enter": leaveCode("anywhere"),
      Enter: chainCommands(leaveCode("emptyLine"), splitListItem(n.listItem)),
      ArrowDown: leaveCode("lastLine"),
      ArrowRight: leaveCode("atEnd"),
      Tab: sinkListItem(n.listItem),
      "Shift-Tab": liftListItem(n.listItem),
    }),
    keymap(baseKeymap),
    history(),
    idPlugin,
  ];
}
