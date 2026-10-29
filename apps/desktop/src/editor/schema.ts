// A ProseMirror schema for MyST.
//
// Each editable MyST type has a node type of the same name, with an `id`
// attribute (the lv-notes node ID) and `extra` (every MyST field the editor
// does not model, carried through untouched). Inline formatting is marks,
// as editors expect; conversion turns marks back into MyST's nested nodes.
// MyST the editor cannot edit yet (directives, tables, figures, citations,
// cross-references, ...) is held verbatim in `opaque` / `opaqueInline` atoms.

import { type DOMOutputSpec, type Mark, type Node as PMNode, Schema } from "prosemirror-model";

const TARGET_ID = /^(doc|node)_[0-9a-f-]{36}$/;

const ided = { id: { default: null }, extra: { default: null } };

/** A short label for MyST the editor shows but does not edit. */
export function opaqueLabel(json: any): string {
  const text = (n: any): string =>
    typeof n?.value === "string" ? n.value : Array.isArray(n?.children) ? n.children.map(text).join("") : "";
  switch (json?.type) {
    case "crossReference":
      return text(json) || json.label || json.identifier || "ref";
    case "cite":
      return `@${json.label ?? json.identifier ?? "?"}`;
    case "citeGroup":
      return (json.children ?? []).map((c: any) => `@${c.label ?? "?"}`).join("; ");
    case "footnoteReference":
      return `[^${json.label ?? json.identifier ?? "?"}]`;
    case "inlineMath":
      return json.value ?? "";
    case "mystRole":
      return `{${json.name}}\`${json.value ?? ""}\``;
    case "mystDirective":
      return `${json.name}${json.args ? ` ${json.args}` : ""}`;
    case "mystTarget":
      return `(${json.label})=`;
    case "mystComment":
      return `% ${json.value ?? ""}`;
    default:
      return text(json).slice(0, 120);
  }
}

function opaqueDOM(node: PMNode, tag: "div" | "span"): DOMOutputSpec {
  const json = node.attrs.json;
  return [
    tag,
    { class: `opaque opaque-${json?.type ?? "unknown"}`, "data-myst": JSON.stringify(json), contenteditable: "false" },
    ["span", { class: "opaque-type" }, json?.type ?? "?"],
    ["span", { class: "opaque-label" }, opaqueLabel(json)],
  ];
}

function parseMyst(dom: HTMLElement) {
  try {
    return { json: JSON.parse(dom.getAttribute("data-myst") ?? "null") };
  } catch {
    return false;
  }
}

const idAttr = (node: PMNode) => (node.attrs.id ? { "data-id": node.attrs.id } : {});
const markExtra = { extra: { default: null } };

export const schema = new Schema({
  nodes: {
    doc: { content: "(block | flow)+", attrs: ided },

    /** MyST `block`: a section of the document, usually invisible. */
    block: {
      content: "flow*",
      attrs: ided,
      toDOM: (n) => ["section", { class: "myst-block", ...idAttr(n) }, 0],
      parseDOM: [{ tag: "section.myst-block" }],
    },

    paragraph: {
      group: "flow",
      content: "inline*",
      // `implicit`: MyST phrasing that sits directly in a container (e.g. a
      // tight list item), shown as a paragraph. It has no MyST node and no ID.
      attrs: { ...ided, implicit: { default: false } },
      toDOM: (n) => ["p", idAttr(n), 0],
      parseDOM: [{ tag: "p" }],
    },

    heading: {
      group: "flow",
      content: "inline*",
      defining: true,
      attrs: { ...ided, depth: { default: 1 } },
      toDOM: (n) => [`h${Math.min(Math.max(n.attrs.depth, 1), 6)}`, idAttr(n), 0],
      parseDOM: [1, 2, 3, 4, 5, 6].map((depth) => ({ tag: `h${depth}`, attrs: { depth } })),
    },

    blockquote: {
      group: "flow",
      content: "flow+",
      defining: true,
      attrs: ided,
      toDOM: (n) => ["blockquote", idAttr(n), 0],
      parseDOM: [{ tag: "blockquote" }],
    },

    list: {
      group: "flow",
      content: "listItem+",
      // `ordered` and `start` are null when MyST leaves them out.
      attrs: { ...ided, ordered: { default: null }, start: { default: null } },
      toDOM: (n) =>
        n.attrs.ordered
          ? ["ol", { ...idAttr(n), ...(n.attrs.start != null ? { start: n.attrs.start } : {}) }, 0]
          : ["ul", idAttr(n), 0],
      parseDOM: [
        { tag: "ul", attrs: { ordered: false } },
        { tag: "ol", getAttrs: (dom) => ({ ordered: true, start: Number((dom as HTMLElement).getAttribute("start") ?? 1) }) },
      ],
    },

    listItem: {
      content: "paragraph flow*",
      defining: true,
      attrs: ided,
      toDOM: (n) => ["li", idAttr(n), 0],
      parseDOM: [{ tag: "li" }],
    },

    admonition: {
      group: "flow",
      content: "admonitionTitle? flow+",
      defining: true,
      attrs: ided,
      toDOM: (n) => ["aside", { class: `admonition admonition-${n.attrs.extra?.kind ?? "note"}`, ...idAttr(n) }, 0],
      parseDOM: [{ tag: "aside.admonition" }],
    },

    admonitionTitle: {
      content: "inline*",
      attrs: ided,
      toDOM: (n) => ["div", { class: "admonition-title", ...idAttr(n) }, 0],
      parseDOM: [{ tag: "div.admonition-title" }],
    },

    code: {
      group: "flow",
      content: "text*",
      marks: "",
      code: true,
      defining: true,
      attrs: { ...ided, lang: { default: null } },
      toDOM: (n) => ["pre", { ...idAttr(n), ...(n.attrs.lang ? { "data-lang": n.attrs.lang } : {}) }, ["code", 0]],
      parseDOM: [{ tag: "pre", preserveWhitespace: "full" }],
    },

    math: {
      group: "flow",
      content: "text*",
      marks: "",
      code: true,
      defining: true,
      attrs: ided,
      // The editor shows it with a node view (math.ts); this is for copying.
      toDOM: (n) => ["pre", { class: "math", ...idAttr(n) }, 0],
      parseDOM: [{ tag: "pre.math", preserveWhitespace: "full", priority: 60 }],
    },

    thematicBreak: {
      group: "flow",
      attrs: ided,
      toDOM: (n) => ["hr", idAttr(n)],
      parseDOM: [{ tag: "hr" }],
    },

    image: {
      group: "flow",
      atom: true,
      draggable: true,
      attrs: { ...ided, url: { default: "" }, alt: { default: null } },
      toDOM: (n) => ["figure", { class: "image", ...idAttr(n) }, ["img", { src: n.attrs.url, alt: n.attrs.alt ?? "" }]],
      parseDOM: [{ tag: "img[src]", getAttrs: (dom) => ({ url: (dom as HTMLElement).getAttribute("src"), alt: (dom as HTMLElement).getAttribute("alt") }) }],
    },

    /** Block-level MyST the editor keeps but does not edit. */
    opaque: {
      group: "flow",
      atom: true,
      selectable: true,
      draggable: true,
      attrs: { json: { default: null } },
      toDOM: (n) => opaqueDOM(n, "div"),
      parseDOM: [{ tag: "div.opaque[data-myst]", getAttrs: (d) => parseMyst(d as HTMLElement) }],
    },

    /** Inline MyST the editor keeps but does not edit (refs, citations, ...). */
    opaqueInline: {
      group: "inline",
      inline: true,
      atom: true,
      selectable: true,
      draggable: true,
      attrs: { json: { default: null } },
      toDOM: (n) => opaqueDOM(n, "span"),
      parseDOM: [{ tag: "span.opaque[data-myst]", getAttrs: (d) => parseMyst(d as HTMLElement) }],
    },

    /** MyST `inlineMath`: its LaTeX source is `value`; shown rendered (math.ts). */
    inlineMath: {
      group: "inline",
      inline: true,
      atom: true,
      selectable: true,
      draggable: true,
      attrs: { value: { default: "" }, extra: { default: null } },
      toDOM: (n) => ["span", { class: "math-inline", "data-latex": n.attrs.value }, n.attrs.value],
      parseDOM: [{ tag: "span.math-inline[data-latex]", getAttrs: (d) => ({ value: (d as HTMLElement).getAttribute("data-latex") ?? "" }) }],
    },

    break: {
      group: "inline",
      inline: true,
      selectable: false,
      attrs: { extra: { default: null } },
      toDOM: () => ["br"],
      parseDOM: [{ tag: "br" }],
    },

    text: { group: "inline" },
  },

  // Order is nesting order on export: earlier marks wrap later ones.
  marks: {
    link: {
      attrs: { id: { default: null }, url: { default: "" }, title: { default: null }, extra: { default: null } },
      inclusive: false,
      // Links to notes and blocks (`doc_…`, `node_…`) are styled apart from web links.
      toDOM: (m: Mark) => [
        "a",
        {
          href: m.attrs.url,
          class: TARGET_ID.test(m.attrs.url) ? `link-internal link-${m.attrs.url.slice(0, m.attrs.url.indexOf("_"))}` : "link-external",
          ...(m.attrs.title ? { title: m.attrs.title } : {}),
        },
        0,
      ],
      parseDOM: [{ tag: "a[href]", getAttrs: (dom) => ({ url: (dom as HTMLElement).getAttribute("href"), title: (dom as HTMLElement).getAttribute("title") }) }],
    },
    strong: { attrs: markExtra, toDOM: () => ["strong", 0], parseDOM: [{ tag: "strong" }, { tag: "b" }] },
    emphasis: { attrs: markExtra, toDOM: () => ["em", 0], parseDOM: [{ tag: "em" }, { tag: "i" }] },
    underline: { attrs: markExtra, toDOM: () => ["u", 0], parseDOM: [{ tag: "u" }] },
    delete: { attrs: markExtra, toDOM: () => ["s", 0], parseDOM: [{ tag: "s" }, { tag: "del" }] },
    subscript: { attrs: markExtra, excludes: "superscript", toDOM: () => ["sub", 0], parseDOM: [{ tag: "sub" }] },
    superscript: { attrs: markExtra, excludes: "subscript", toDOM: () => ["sup", 0], parseDOM: [{ tag: "sup" }] },
    /** MyST `inlineCode`. */
    inlineCode: { attrs: markExtra, code: true, toDOM: () => ["code", 0], parseDOM: [{ tag: "code" }] },
  },
});

/** MyST phrasing types with a mark of the same name. */
export const MARK_TYPES = ["strong", "emphasis", "underline", "delete", "subscript", "superscript"] as const;
