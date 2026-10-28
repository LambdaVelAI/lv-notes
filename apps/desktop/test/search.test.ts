import { describe, expect, it } from "vitest";
import { memoryBackend } from "../src/backend";
import { highlight, snippet } from "../src/search";

const marked = (text: string, words: string[]) =>
  highlight(text, words)
    .map((s) => (s.match ? `[${s.text}]` : s.text))
    .join("");

describe("highlight", () => {
  it("marks the matching start of each word", () => {
    expect(marked("Measurable sets, measure theory.", ["measur", "theor"])).toBe("[Measur]able sets, [measur]e [theor]y.");
  });

  it("marks the longest matching prefix, and leaves other text alone", () => {
    expect(marked("theorems", ["the", "theorem"])).toBe("[theorem]s");
    expect(marked("No match here", ["xyz"])).toBe("No match here");
    expect(marked("Ünïcode wörds", ["wö"])).toBe("Ünïcode [wö]rds");
  });
});

describe("snippet", () => {
  const long = `${"filler ".repeat(60)}the key result ${"tail ".repeat(60)}`;

  it("keeps short text whole, with whitespace collapsed", () => {
    expect(snippet("  a\n  b ", ["a"])).toBe("a b");
  });

  it("starts a little before the first match, on a word boundary", () => {
    const s = snippet(long, ["key"], 120);
    expect(s.startsWith("…filler")).toBe(true);
    expect(s.endsWith("tail…")).toBe(true);
    expect(s).toContain("the key result");
    expect(s.length).toBeLessThanOrEqual(122);
  });
});

describe("memory backend search", () => {
  it("finds notes by title and blocks across formatting", async () => {
    const backend = memoryBackend([
      { title: "Measure theory", myst: { type: "root", children: [{ type: "paragraph", id: "node_a", children: [{ type: "text", value: "Every measurable " }, { type: "strong", children: [{ type: "text", value: "theorem" }] }] }] } },
      { title: "Other", myst: { type: "root", children: [{ type: "paragraph", id: "node_b", children: [{ type: "text", value: "Nothing here." }] }] } },
    ]);
    const found = await backend.search("measur theor");
    expect(found.documents.map((d) => d.title)).toEqual(["Measure theory"]);
    expect(found.blocks.map((b) => b.node)).toEqual(["node_a"]);
    expect(found.more).toBe(false);
    expect(await backend.search(" - ")).toEqual({ documents: [], blocks: [], more: false });
  });
});
