import { Node as PMNode } from "prosemirror-model";
import { EditorState, TextSelection } from "prosemirror-state";
import { describe, expect, it } from "vitest";
import { isNodeId, newNodeId } from "../src/editor/ids";
import { type Myst, mystIds, mystToPm, pmToMyst } from "../src/editor/myst";
import { assignIds, leaveCode, plugins, renameIds } from "../src/editor/plugins";
import { schema } from "../src/editor/schema";

const fixtures = import.meta.glob<{ default: Myst }>("../../../crates/lv-core/tests/fixtures/myst/*.json", { eager: true });
const named = Object.entries(fixtures).map(([path, mod]) => [path.split("/").pop()!, mod.default] as const);

/** The same document as fresh objects, so nothing comes from the load cache. */
const uncached = (doc: PMNode) => PMNode.fromJSON(schema, doc.toJSON());

const state = (myst: Myst) => EditorState.create({ doc: mystToPm(myst), plugins: plugins() });

describe("fixtures", () => {
  it.each(named)("%s loads into a valid document", (_, myst) => {
    expect(() => mystToPm(myst).check()).not.toThrow();
  });

  it.each(named)("%s saves back unchanged when untouched", (_, myst) => {
    expect(pmToMyst(mystToPm(myst))).toBe(myst);
  });

  it.each(named)("%s rebuilt from the editor model is stable", (_, myst) => {
    const doc = mystToPm(myst);
    const rebuilt = pmToMyst(uncached(doc));
    expect(mystToPm(rebuilt).eq(doc)).toBe(true);
    expect(pmToMyst(uncached(mystToPm(rebuilt)))).toEqual(rebuilt);
  });
});

describe("conversion of edited content", () => {
  const id = () => newNodeId();

  it("rebuilds nested phrasing from marks", () => {
    const [p, l] = [id(), id()];
    const myst: Myst = {
      type: "root",
      id: id(),
      children: [
        {
          type: "paragraph",
          id: p,
          children: [
            { type: "text", value: "A " },
            { type: "strong", children: [{ type: "text", value: "bold " }, { type: "emphasis", children: [{ type: "text", value: "both" }] }] },
            { type: "text", value: ", " },
            { type: "link", id: l, url: "https://example.org", children: [{ type: "inlineCode", value: "code" }, { type: "text", value: " link" }] },
            { type: "break" },
            { type: "crossReference", identifier: "fig-1", label: "fig-1", children: [{ type: "text", value: "Figure 1" }] },
          ],
        },
      ],
    };
    expect(pmToMyst(uncached(mystToPm(myst)))).toEqual(myst);
  });

  it("keeps tight list items' text without inventing paragraphs", () => {
    const myst: Myst = {
      type: "root",
      id: id(),
      children: [
        {
          type: "list",
          id: id(),
          ordered: true,
          start: 3,
          children: [
            { type: "listItem", id: id(), children: [{ type: "text", value: "one" }] },
            {
              type: "listItem",
              id: id(),
              children: [
                { type: "text", value: "two" },
                { type: "list", id: id(), children: [{ type: "listItem", id: id(), children: [{ type: "text", value: "nested" }] }] },
              ],
            },
          ],
        },
      ],
    };
    expect(pmToMyst(uncached(mystToPm(myst)))).toEqual(myst);
  });

  it("holds what it cannot edit verbatim", () => {
    const table = { type: "table", id: id(), children: [{ type: "tableRow", children: [{ type: "tableCell", children: [{ type: "text", value: "x" }] }] }] };
    const myst: Myst = { type: "root", id: id(), children: [table] };
    const doc = mystToPm(myst);
    expect(doc.firstChild!.type.name).toBe("opaque");
    expect(pmToMyst(uncached(doc)).children[0]).toBe(table);
  });

  it("changes only what was edited", () => {
    const [fixture] = named.find(([n]) => n === "prose.json")!.slice(1) as [Myst];
    let s = state(fixture);
    s = s.apply(assignIds(s)!);
    const before = pmToMyst(s.doc);
    // Type in the first heading.
    s = s.apply(s.tr.insertText("!", 3));
    const after = pmToMyst(s.doc);
    expect(after).not.toBe(before);
    const blocksBefore = before.children[0].children;
    const blocksAfter = after.children[0].children;
    expect(blocksAfter[0]).not.toBe(blocksBefore[0]);
    blocksAfter.slice(1).forEach((b: Myst, i: number) => expect(b).toBe(blocksBefore[i + 1]));
  });
});

describe("node IDs", () => {
  it("are UUIDv7 in lv-core's form, in order", () => {
    const ids = Array.from({ length: 50 }, newNodeId);
    ids.forEach((i) => expect(isNodeId(i)).toBe(true));
    expect([...ids].sort()).toEqual(ids);
    expect(ids[0][19]).toBe("7");
  });

  it("are assigned to new nodes and deduplicated on split", () => {
    let s = state({ type: "root", children: [{ type: "paragraph", children: [{ type: "text", value: "hello world" }] }] });
    s = s.apply(assignIds(s)!);
    const first = s.doc.firstChild!.attrs.id;
    expect(isNodeId(s.doc.attrs.id) && isNodeId(first)).toBe(true);

    // Split the paragraph: ProseMirror copies its attrs, so both halves start with the same ID.
    s = s.apply(s.tr.setSelection(TextSelection.create(s.doc, 6)).split(6));
    const ids = [s.doc.child(0).attrs.id, s.doc.child(1).attrs.id];
    expect(ids[0]).toBe(first);
    expect(isNodeId(ids[1]) && ids[1] !== first).toBe(true);
  });

  it("are freshened inside duplicated opaque nodes and links", () => {
    const table = { type: "table", id: newNodeId(), children: [] };
    const link = { type: "link", id: newNodeId(), url: "u", children: [{ type: "text", value: "a" }] };
    let s = state({
      type: "root",
      id: newNodeId(),
      children: [table, table, { type: "paragraph", id: newNodeId(), children: [link, { type: "text", value: " " }, link] }],
    });
    s = s.apply(assignIds(s)!);
    const all: string[] = [];
    s.doc.descendants((n) => {
      if (n.type.name === "opaque") all.push(...mystIds(n.attrs.json));
      else if (n.attrs.id) all.push(n.attrs.id);
      n.marks.forEach((m) => m.attrs.id && all.push(m.attrs.id));
    });
    const json = pmToMyst(s.doc);
    expect(mystIds(json).length).toBe(new Set(mystIds(json)).size);
    expect(mystIds(json)).toContain(table.id);
    expect(mystIds(json)).toContain(link.id);
  });

  it("are renamed as the core asks", () => {
    const [old, fresh] = [newNodeId(), newNodeId()];
    let s = state({ type: "root", id: newNodeId(), children: [{ type: "paragraph", id: old, children: [] }] });
    s = s.apply(renameIds(s, [[old, fresh]])!);
    expect(s.doc.firstChild!.attrs.id).toBe(fresh);
    expect(pmToMyst(s.doc).children[0].id).toBe(fresh);
  });
});

describe("leaving a code block", () => {
  const codeAtEnd = (value: string) => {
    let s = state({ type: "root", children: [{ type: "code", value }] });
    s = s.apply(assignIds(s)!);
    return s.apply(s.tr.setSelection(TextSelection.create(s.doc, 1 + value.length)));
  };
  const run = (s: EditorState, command: ReturnType<typeof leaveCode>) => {
    let next: EditorState | null = null;
    const handled = command(s, (tr) => (next = s.apply(tr)));
    return { handled, next: next as EditorState | null };
  };
  const shape = (s: EditorState) => s.doc.content.content.map((n) => `${n.type.name}:${JSON.stringify(n.textContent)}`);

  it("Enter on an empty last line leaves and drops the line", () => {
    const { handled, next } = run(codeAtEnd("x = 1\n"), leaveCode("emptyLine"));
    expect(handled).toBe(true);
    expect(shape(next!)).toEqual(['code:"x = 1"', 'paragraph:""']);
    expect(next!.selection.$head.parent.type.name).toBe("paragraph");
  });

  it("Enter elsewhere is a newline", () => {
    expect(run(codeAtEnd("x = 1"), leaveCode("emptyLine")).handled).toBe(false);
  });

  it("an arrow at the end of the note leaves; with something after, it moves as usual", () => {
    const { next } = run(codeAtEnd("x = 1"), leaveCode("atEnd"));
    expect(shape(next!)).toEqual(['code:"x = 1"', 'paragraph:""']);

    let s = state({ type: "root", children: [{ type: "code", value: "x" }, { type: "paragraph", children: [{ type: "text", value: "after" }] }] });
    s = s.apply(s.tr.setSelection(TextSelection.create(s.doc, 2)));
    expect(run(s, leaveCode("atEnd")).handled).toBe(false);
  });

  it("does nothing outside code", () => {
    const s = state({ type: "root", children: [{ type: "paragraph", children: [{ type: "text", value: "p" }] }] });
    expect(run(s, leaveCode("anywhere")).handled).toBe(false);
  });
});
