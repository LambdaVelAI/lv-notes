import { EditorState, TextSelection } from "prosemirror-state";
import { describe, expect, it } from "vitest";
import { memoryBackend } from "../src/backend";
import { newNodeId } from "../src/editor/ids";
import { BROKEN, LINK_REQUEST, brokenLinksPlugin, setLinkRelation, findId, internalTargets, isInternalTarget, linkAt, removeLink, requestLink, setLink } from "../src/editor/links";
import { type Myst, mystToPm, pmToMyst } from "../src/editor/myst";
import { plugins } from "../src/editor/plugins";
import { shorten } from "../src/linking";

const doc = "doc_01a17fe6-3636-7430-a1fe-a8b3500bb89a";
const block = "node_01a17fe6-3636-7430-a1fe-a8b6c510ecd3";

const paragraph = (...children: Myst[]): Myst => ({ type: "root", children: [{ type: "paragraph", id: newNodeId(), children }] });

function state(myst: Myst, from: number, to = from) {
  const s = EditorState.create({ doc: mystToPm(myst), plugins: plugins() });
  return s.apply(s.tr.setSelection(TextSelection.create(s.doc, from, to)));
}

function run(s: EditorState, command: (s: EditorState, d?: (tr: any) => void) => boolean): EditorState {
  let next = s;
  expect(command(s, (tr) => (next = s.apply(tr)))).toBe(true);
  return next;
}

/** The paragraph's phrasing as saved. */
const saved = (s: EditorState) => pmToMyst(s.doc).children[0].children;

describe("links", () => {
  it("recognizes note and block targets", () => {
    expect(isInternalTarget(doc)).toBe(true);
    expect(isInternalTarget(block)).toBe(true);
    expect(isInternalTarget("https://example.org")).toBe(false);
    expect(isInternalTarget("#label")).toBe(false);
    expect(isInternalTarget("doc_nope")).toBe(false);
  });

  it("links the selection to a note, with a new link ID", () => {
    // "Slow reading" with "reading" (5..12) selected.
    const s = run(state(paragraph({ type: "text", value: "Slow reading" }), 6, 13), setLink(doc));
    const [text, link] = saved(s);
    expect(text).toEqual({ type: "text", value: "Slow " });
    expect(link).toMatchObject({ type: "link", url: doc, children: [{ type: "text", value: "reading" }] });
    expect(link.id).toMatch(/^node_/);
  });

  it("inserts the target's text at a bare cursor", () => {
    const s = run(state(paragraph({ type: "text", value: "See " }), 5), setLink(block, "the claim"));
    expect(saved(s)[1]).toMatchObject({ type: "link", url: block, children: [{ type: "text", value: "the claim" }] });
    expect(s.selection.from).toBe(5 + "the claim".length);
  });

  it("edits the link at the cursor in place, keeping its ID", () => {
    const id = newNodeId();
    const myst = paragraph({ type: "text", value: "a " }, { type: "link", id, url: doc, children: [{ type: "text", value: "note" }] });
    const at = state(myst, 5);
    expect(linkAt(at)).toMatchObject({ from: 3, to: 7 });
    const s = run(at, setLink(block));
    expect(saved(s)[1]).toMatchObject({ type: "link", id, url: block, children: [{ type: "text", value: "note" }] });
    // At the end of the link counts; before its first letter does not.
    expect(linkAt(state(myst, 7))).not.toBeNull();
    expect(linkAt(state(myst, 3))).toBeNull();
  });

  it("types and untypes the link at the cursor", () => {
    const id = newNodeId();
    const myst = paragraph({ type: "link", id, url: doc, children: [{ type: "text", value: "note" }] });
    let s = run(state(myst, 2), setLinkRelation("supports"));
    expect(saved(s)[0]).toMatchObject({ type: "link", id, url: doc, relation: "supports" });
    s = run(s, setLinkRelation(null));
    expect(saved(s)[0]).not.toHaveProperty("relation");
    expect(setLinkRelation("supports")(state(paragraph({ type: "text", value: "x" }), 1))).toBe(false);
  });

  it("removes the link at the cursor", () => {
    const myst = paragraph({ type: "link", id: newNodeId(), url: doc, children: [{ type: "text", value: "note" }] });
    const s = run(state(myst, 2), removeLink);
    expect(saved(s)).toEqual([{ type: "text", value: "note" }]);
    expect(removeLink(s)).toBe(false);
  });

  it("finds blocks and links by ID", () => {
    const [p, l] = [newNodeId(), newNodeId()];
    const myst: Myst = {
      type: "root",
      children: [{ type: "paragraph", id: p, children: [{ type: "text", value: "x " }, { type: "link", id: l, url: doc, children: [{ type: "text", value: "y" }] }] }],
    };
    const d = mystToPm(myst);
    expect(findId(d, p)).toMatchObject({ pos: 0 });
    expect(findId(d, l)).toMatchObject({ pos: 3, to: 4 });
    expect(findId(d, newNodeId())).toBeNull();
  });

  it("⌘K asks for the picker, except in code", () => {
    let tr: any;
    expect(requestLink(state(paragraph({ type: "text", value: "x" }), 1), (t) => (tr = t))).toBe(true);
    expect(tr.getMeta(LINK_REQUEST)).toBe(true);
    expect(requestLink(state({ type: "root", children: [{ type: "code", id: newNodeId(), value: "x" }] }, 1))).toBe(false);
  });

  it("lists link targets and marks the broken ones", () => {
    const myst = paragraph(
      { type: "link", id: newNodeId(), url: doc, children: [{ type: "text", value: "ok" }] },
      { type: "text", value: " " },
      { type: "link", id: newNodeId(), url: block, children: [{ type: "text", value: "gone" }] },
      { type: "link", id: newNodeId(), url: "https://example.org", children: [{ type: "text", value: "web" }] },
    );
    let s = state(myst, 1);
    expect(internalTargets(s.doc).sort()).toEqual([doc, block].sort());
    s = s.apply(s.tr.setMeta(BROKEN, new Set([block])));
    const marked = () => brokenLinksPlugin.getState(s)!.decorations.find().map((d) => s.doc.textBetween(d.from, d.to));
    expect(marked()).toEqual(["gone"]);
    // Edits keep the marks on the same links.
    s = s.apply(s.tr.insertText("x", 1));
    expect(marked()).toEqual(["gone"]);
  });

  it("shortens block text for labels", () => {
    expect(shorten("  a\n b ")).toBe("a b");
    expect(shorten("x".repeat(70), 10)).toBe(`${"x".repeat(9)}…`);
  });
});

describe("the in-memory backend's link queries", () => {
  it("answers backlinks, search and locate like the core", async () => {
    const backend = memoryBackend();
    const { doc: target } = await backend.createDocument("Target");
    const { doc: source } = await backend.createDocument("Source");
    const [p, l1, l2] = [newNodeId(), newNodeId(), newNodeId()];
    await backend.saveDocument(target, 0, paragraph({ type: "text", value: "Marginalia argue" }));
    const loaded = await backend.loadDocument(target);
    const blockId = loaded.myst.children[0].id;
    await backend.saveDocument(source, 0, {
      type: "root",
      children: [
        {
          type: "paragraph",
          id: p,
          children: [
            { type: "link", id: l1, url: target, children: [{ type: "text", value: "note" }] },
            { type: "link", id: l2, url: blockId, children: [{ type: "text", value: "block" }] },
          ],
        },
      ],
    });

    const links = await backend.backlinks(target);
    expect(links.map((b) => [b.source, b.target, b.context])).toEqual([
      [l1, null, "noteblock"],
      [l2, blockId, "noteblock"],
    ]);
    expect(await backend.backlinks(source)).toEqual([]);
    expect((await backend.searchBlocks("margin arg")).map((h) => h.node)).toEqual([blockId]);
    expect(await backend.locate(blockId)).toMatchObject({ doc: target, block: { text: "Marginalia argue" } });
    expect(await backend.locate(target)).toMatchObject({ doc: target, title: "Target", block: null });
    expect(await backend.locate(newNodeId())).toBeNull();
  });
});
