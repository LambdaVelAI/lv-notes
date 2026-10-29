import { EditorState, NodeSelection, TextSelection } from "prosemirror-state";
import { describe, expect, it } from "vitest";
import { INLINE_MATH_RULE, insertInlineMath, labelIdentifier, withLabel } from "../src/editor/math";
import { type Myst, mystToPm, pmToMyst } from "../src/editor/myst";
import { commands, plugins } from "../src/editor/plugins";
import { schema } from "../src/editor/schema";

const para = "node_01a17fe6-3636-7430-a1fe-a8b6c510ecd3";
const eq = "node_01a17fe6-3636-7430-a1fe-a8b6c510ecd4";

const doc = (children: Myst[]): Myst => ({ type: "root", children });
const state = (myst: Myst) => EditorState.create({ doc: mystToPm(myst), plugins: plugins() });

describe("math conversion", () => {
  it("inline math is a node, and round-trips with its unmodelled fields", () => {
    const myst = doc([
      { type: "paragraph", id: para, children: [{ type: "text", value: "Let " }, { type: "inlineMath", value: "x^2", position: { start: 1 } }, { type: "text", value: "." }] },
    ]);
    const pm = mystToPm(myst);
    const node = pm.firstChild!.child(1);
    expect(node.type).toBe(schema.nodes.inlineMath);
    expect(node.attrs.value).toBe("x^2");

    // Edited: rebuilt from the model, keeping the extra fields.
    const edited = pm.firstChild!.copy(pm.firstChild!.content.replaceChild(1, node.type.create({ ...node.attrs, value: "y" })));
    expect(pmToMyst(edited).children[1]).toEqual({ type: "inlineMath", position: { start: 1 }, value: "y" });
  });

  it("a display equation keeps its label", () => {
    const myst = doc([{ type: "math", id: eq, value: "E = mc^2", label: "eq-1", identifier: "eq-1" }]);
    const pm = mystToPm(myst);
    const rebuilt = pm.copy(pm.content);
    expect(pmToMyst(rebuilt).children[0]).toEqual({ type: "math", label: "eq-1", identifier: "eq-1", id: eq, value: "E = mc^2" });
  });
});

describe("labels", () => {
  it("normalize like the core", () => {
    expect(labelIdentifier("  Eq  Energy ")).toBe("eq energy");
  });

  it("set and clear label and identifier together", () => {
    expect(withLabel({ enumerated: true }, "Eq-A")).toEqual({ enumerated: true, label: "Eq-A", identifier: "eq-a" });
    expect(withLabel({ label: "x", identifier: "x" }, " ")).toBeNull();
  });
});

describe("the $…$ rule", () => {
  const match = (text: string) => INLINE_MATH_RULE.exec(text)?.[2] ?? null;

  it("matches math, closed by the typed $", () => {
    expect(match("Let $x^2$")).toBe("x^2");
    expect(match("$a$")).toBe("a");
    expect(match("so $\\frac{a}{b}$")).toBe("\\frac{a}{b}");
  });

  it("leaves prices, escaped dollars and $$ alone", () => {
    expect(match("costs $5 or $")).toBeNull();
    expect(match("from $ 5$")).toBeNull();
    expect(match("an escaped \\$x$")).toBeNull();
    expect(match("$$")).toBeNull();
  });
});

describe("math commands", () => {
  it("turns the selection into inline math, selected for editing", () => {
    let s = state(doc([{ type: "paragraph", id: para, children: [{ type: "text", value: "Let x squared be" }] }]));
    s = s.apply(s.tr.setSelection(TextSelection.create(s.doc, 5, 14)));
    insertInlineMath(s, (tr) => (s = s.apply(tr)));
    expect(s.selection).toBeInstanceOf(NodeSelection);
    expect((s.selection as NodeSelection).node.attrs.value).toBe("x squared");
    expect(pmToMyst(s.doc).children[0].children.map((c: Myst) => c.type)).toEqual(["text", "inlineMath", "text"]);
  });

  it("does nothing inside an equation", () => {
    let s = state(doc([{ type: "math", id: eq, value: "x" }]));
    s = s.apply(s.tr.setSelection(TextSelection.create(s.doc, 2)));
    expect(insertInlineMath(s)).toBe(false);
  });

  it("changing a paragraph to an equation keeps its ID", () => {
    let s = state(doc([{ type: "paragraph", id: para, children: [{ type: "text", value: "x^2" }] }]));
    s = s.apply(s.tr.setSelection(TextSelection.create(s.doc, 2)));
    commands.math(s, (tr) => (s = s.apply(tr)));
    expect(pmToMyst(s.doc).children[0]).toEqual({ type: "math", id: para, value: "x^2" });
  });
});
