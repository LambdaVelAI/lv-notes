// Records what the editor sends the core during real ProseMirror edits.
//
// Each trace is a list of native MyST trees: the loaded document, then the
// tree after each edit. `WRITE_TRACES=1 npm test` writes them to
// test/traces/, where the Rust test `editor_traces` replays them through a
// Session and checks every save comes back exactly. Regenerate them when
// the schema or conversion changes.

import { writeFileSync, mkdirSync } from "node:fs";
import { setBlockType, toggleMark, joinBackward } from "prosemirror-commands";
import { wrapInList } from "prosemirror-schema-list";
import { type Command, EditorState, TextSelection } from "prosemirror-state";
import { describe, expect, it } from "vitest";
import { removeLink, setLink } from "../src/editor/links";
import { type Myst, mystToPm, pmToMyst } from "../src/editor/myst";
import { assignIds, plugins } from "../src/editor/plugins";
import { schema } from "../src/editor/schema";

const fixtures = import.meta.glob<{ default: Myst }>("../../../crates/lv-core/tests/fixtures/myst/*.json", { eager: true });

type Edit = [string, (s: EditorState) => EditorState | null];

/** Position just inside the first textblock with text, if any. */
function firstText(s: EditorState, type = schema.nodes.paragraph): number | null {
  let found: number | null = null;
  s.doc.descendants((node, pos) => {
    if (found != null) return false;
    if (node.type === type && !node.attrs.implicit && node.textContent.length > 3) found = pos + 1;
    return true;
  });
  return found;
}

const at = (s: EditorState, pos: number, to = pos) => s.apply(s.tr.setSelection(TextSelection.create(s.doc, pos, to)));
const run = (s: EditorState, command: Command) => {
  let next: EditorState | null = null;
  command(s, (tr) => (next = s.apply(tr)));
  return next;
};

const EDITS: Edit[] = [
  ["type at the start of a paragraph", (s) => { const p = firstText(s); return p == null ? null : s.apply(s.tr.insertText("Indeed, ", p)); }],
  ["type in a heading", (s) => { const p = firstText(s, schema.nodes.heading); return p == null ? null : s.apply(s.tr.insertText("§ ", p)); }],
  ["bold a word", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p, p + 3), toggleMark(schema.marks.strong)); }],
  ["link a word to its block", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p, p + 3), setLink(s.doc.resolve(p).parent.attrs.id)); }],
  ["insert a link to a note", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p), setLink("doc_01a17fe6-3636-7430-a1fe-a8b3500bb89a", "a note")); }],
  ["retarget the link", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p + 2), setLink("https://example.org")); }],
  ["remove the link", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p + 2), removeLink); }],
  ["split a paragraph", (s) => { const p = firstText(s); return p == null ? null : s.apply(s.tr.split(p + 2)); }],
  ["join it back", (s) => { const p = firstText(s); if (p == null) return null; const next = s.doc.resolve(p).after() + 1; return run(at(s, next + 1), joinBackward); }],
  ["paragraph to heading", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p), setBlockType(schema.nodes.heading, { depth: 2 })); }],
  ["wrap in a list", (s) => { const p = firstText(s); return p == null ? null : run(at(s, p), wrapInList(schema.nodes.list)); }],
  ["new paragraph at the end", (s) => s.apply(s.tr.insert(s.doc.content.size, schema.nodes.paragraph.create(null, schema.text("The end.")))) ],
  ["move the last block first", (s) => {
    const last = s.doc.lastChild!;
    const tr = s.tr.delete(s.doc.content.size - last.nodeSize, s.doc.content.size).insert(0, last);
    return s.apply(tr);
  }],
  ["delete across blocks", (s) => { const p = firstText(s); if (p == null) return null; const end = Math.min(p + 40, s.doc.content.size - 2); return s.apply(s.tr.delete(p, end)); }],
];

function trace(myst: Myst): { edit: string; myst: Myst }[] {
  let s = EditorState.create({ doc: mystToPm(myst), plugins: plugins() });
  s = s.apply(assignIds(s)!);
  const out = [{ edit: "load", myst: pmToMyst(s.doc) }];
  for (const [edit, apply] of EDITS) {
    const next = apply(s);
    if (!next || next.doc.eq(s.doc)) continue;
    next.doc.check();
    s = next;
    out.push({ edit, myst: pmToMyst(s.doc) });
  }
  return out;
}

describe("editor traces", () => {
  for (const [path, mod] of Object.entries(fixtures)) {
    const name = path.split("/").pop()!.replace(".json", "");
    it(`${name} edits produce MyST`, () => {
      const steps = trace(mod.default);
      expect(steps.length).toBeGreaterThan(2);
      if (process.env.WRITE_TRACES) {
        const dir = new URL("./traces/", import.meta.url);
        mkdirSync(dir, { recursive: true });
        writeFileSync(new URL(`${name}.json`, dir), JSON.stringify(steps, null, 1) + "\n");
      }
    });
  }
});
