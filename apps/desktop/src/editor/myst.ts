// MyST JSON <-> ProseMirror documents.
//
// Loading remembers, for every ProseMirror node built from MyST, the MyST it
// came from. ProseMirror nodes are immutable and shared between document
// versions, so on save an untouched node is the very same object and
// converts back to exactly the MyST it was loaded from, source positions and
// unmodelled fields included. Only nodes the user actually changed are
// rebuilt from the editor's model, and only those produce ops in the core.

import { type Mark, type Node as PMNode } from "prosemirror-model";
import { MARK_TYPES, schema } from "./schema";

export type Myst = { type: string; [field: string]: any };

const source = new WeakMap<PMNode, Myst>();

/** MyST types that are phrasing (inline) content. */
const PHRASING = new Set([
  "text", "emphasis", "strong", "underline", "delete", "subscript", "superscript", "inlineCode", "inlineMath",
  "link", "break", "crossReference", "cite", "citeGroup", "footnoteReference", "mystRole", "abbreviation",
]);

/** Fields of `json` other than structure and those the node models itself. */
function extra(json: Myst, modelled: string[] = []): Record<string, any> | null {
  const skip = new Set(["type", "id", "children", ...modelled]);
  const rest = Object.fromEntries(Object.entries(json).filter(([k]) => !skip.has(k)));
  return Object.keys(rest).length ? rest : null;
}

const remember = (node: PMNode, json: Myst) => (source.set(node, json), node);

/** A ProseMirror document for a MyST `root`. */
export function mystToPm(root: Myst): PMNode {
  let children = flow(root.children ?? []);
  if (!children.length) children = [schema.nodes.paragraph.create()];
  const doc = schema.nodes.doc.create({ id: root.id ?? null, extra: extra(root) }, children);
  try {
    doc.check();
  } catch {
    // Content this schema cannot hold at the top level: keep it all opaque.
    return schema.nodes.doc.create({ id: root.id ?? null, extra: extra(root) }, (root.children ?? []).map(opaque));
  }
  return remember(doc, root);
}

function opaque(json: Myst): PMNode {
  return schema.nodes.opaque.create({ json });
}

/** Flow content; runs of phrasing become implicit paragraphs. */
function flow(children: Myst[]): PMNode[] {
  const out: PMNode[] = [];
  let run: Myst[] = [];
  const flush = () => {
    if (run.length) out.push(schema.nodes.paragraph.create({ implicit: true }, inline(run)));
    run = [];
  };
  for (const child of children) {
    if (PHRASING.has(child.type)) {
      run.push(child);
    } else {
      flush();
      out.push(flowNode(child));
    }
  }
  flush();
  return out;
}

function flowNode(json: Myst): PMNode {
  const n = schema.nodes;
  const id = json.id ?? null;
  const kids = json.children ?? [];
  let node: PMNode;
  switch (json.type) {
    case "block":
    case "blockquote":
    case "listItem":
    case "admonition":
      node = n[json.type].create({ id, extra: extra(json) }, flow(kids));
      break;
    case "paragraph":
    case "admonitionTitle":
      node = n[json.type].create({ id, extra: extra(json) }, inline(kids));
      break;
    case "heading":
      node = n.heading.create({ id, depth: json.depth ?? 1, extra: extra(json, ["depth"]) }, inline(kids));
      break;
    case "list":
      node = n.list.create(
        { id, ordered: json.ordered ?? null, start: json.start ?? null, extra: extra(json, ["ordered", "start"]) },
        kids.map(flowNode),
      );
      break;
    case "code":
      node = n.code.create({ id, lang: json.lang ?? null, extra: extra(json, ["lang", "value"]) }, text(json.value));
      break;
    case "math":
      node = n.math.create({ id, extra: extra(json, ["value"]) }, text(json.value));
      break;
    case "thematicBreak":
      node = n.thematicBreak.create({ id, extra: extra(json) });
      break;
    case "image":
      node = n.image.create({ id, url: json.url ?? "", alt: json.alt ?? null, extra: extra(json, ["url", "alt"]) });
      break;
    default:
      return opaque(json);
  }
  try {
    node.check();
  } catch {
    return opaque(json);
  }
  return remember(node, json);
}

const text = (value: unknown, marks: readonly Mark[] = []) =>
  typeof value === "string" && value ? [schema.text(value, marks)] : [];

function inline(children: Myst[], marks: readonly Mark[] = []): PMNode[] {
  const m = schema.marks;
  return children.flatMap((json): PMNode[] => {
    switch (json.type) {
      case "text":
        return text(json.value, marks);
      case "inlineCode":
        return text(json.value, m.inlineCode.create({ extra: extra(json, ["value"]) }).addToSet(marks));
      case "break":
        return [schema.nodes.break.create({ extra: extra(json) }, null, marks)];
      case "inlineMath":
        return [schema.nodes.inlineMath.create({ value: json.value ?? "", extra: extra(json, ["value"]) }, null, marks)];
      case "link": {
        const link = m.link.create({ id: json.id ?? null, url: json.url ?? "", title: json.title ?? null, extra: extra(json, ["url", "title"]) });
        return inline(json.children ?? [], link.addToSet(marks));
      }
      default:
        if ((MARK_TYPES as readonly string[]).includes(json.type)) {
          return inline(json.children ?? [], m[json.type].create({ extra: extra(json) }).addToSet(marks));
        }
        return [schema.nodes.opaqueInline.create({ json }, null, marks)];
    }
  });
}

// ---- Back to MyST ---------------------------------------------------------

const base = (node: PMNode): Record<string, any> => ({ ...(node.attrs.extra ?? {}), ...(node.attrs.id ? { id: node.attrs.id } : {}) });

/** The MyST for `doc` (a `root`) or any node in it. */
export function pmToMyst(node: PMNode): Myst {
  const known = source.get(node);
  if (known) return known;
  const a = node.attrs;
  let json: Myst;
  switch (node.type.name) {
    case "doc":
      json = { type: "root", ...base(node), children: flowOut(node) };
      break;
    case "block":
    case "blockquote":
    case "listItem":
    case "admonition":
      json = { type: node.type.name, ...base(node), children: flowOut(node) };
      break;
    case "paragraph":
    case "admonitionTitle":
      json = { type: node.type.name, ...base(node), children: inlineOut(node) };
      break;
    case "heading":
      json = { type: "heading", ...base(node), depth: a.depth, children: inlineOut(node) };
      break;
    case "list":
      json = {
        type: "list",
        ...base(node),
        ...(a.ordered != null ? { ordered: a.ordered } : {}),
        ...(a.start != null ? { start: a.start } : {}),
        children: flowOut(node),
      };
      break;
    case "code":
      json = { type: "code", ...base(node), ...(a.lang ? { lang: a.lang } : {}), value: node.textContent };
      break;
    case "math":
      json = { type: "math", ...base(node), value: node.textContent };
      break;
    case "thematicBreak":
      json = { type: "thematicBreak", ...base(node) };
      break;
    case "image":
      json = { type: "image", ...base(node), url: a.url, ...(a.alt != null ? { alt: a.alt } : {}) };
      break;
    case "opaque":
    case "opaqueInline":
      return a.json;
    default:
      throw new Error(`no MyST for a ${node.type.name}`);
  }
  source.set(node, json);
  return json;
}

function flowOut(node: PMNode): Myst[] {
  const out: Myst[] = [];
  node.forEach((child) => {
    if (child.type === schema.nodes.paragraph && child.attrs.implicit) out.push(...inlineOut(child));
    else out.push(pmToMyst(child));
  });
  return out;
}

function markToMyst(mark: Mark): Myst {
  const a = mark.attrs;
  if (mark.type === schema.marks.link) {
    return { type: "link", ...(a.extra ?? {}), ...(a.id ? { id: a.id } : {}), url: a.url, ...(a.title ? { title: a.title } : {}), children: [] };
  }
  return { type: mark.type.name, ...(a.extra ?? {}), children: [] };
}

/** Inline content back to nested MyST phrasing: marks shared by neighbours
 *  become one node around them. */
function inlineOut(node: PMNode): Myst[] {
  const out: Myst[] = [];
  const open: { mark: Mark; json: Myst }[] = [];
  const into = () => (open.length ? open[open.length - 1].json.children : out);
  node.forEach((child) => {
    const code = child.marks.find((m) => m.type === schema.marks.inlineCode);
    const marks = child.marks.filter((m) => m !== code);
    let keep = 0;
    while (keep < open.length && keep < marks.length && open[keep].mark.eq(marks[keep])) keep++;
    open.length = keep;
    for (const mark of marks.slice(keep)) {
      const json = markToMyst(mark);
      into().push(json);
      open.push({ mark, json });
    }
    if (child.isText) {
      into().push(code ? { type: "inlineCode", ...(code.attrs.extra ?? {}), value: child.text } : { type: "text", value: child.text });
    } else if (child.type === schema.nodes.break) {
      into().push({ type: "break", ...(child.attrs.extra ?? {}) });
    } else if (child.type === schema.nodes.inlineMath) {
      into().push({ type: "inlineMath", ...(child.attrs.extra ?? {}), value: child.attrs.value });
    } else {
      into().push(child.attrs.json);
    }
  });
  return out;
}

// ---- IDs inside MyST ----------------------------------------------------------

/** Formatting and text: the MyST nodes that carry no ID (lv-core's TEXT_RUN_TYPES). */
export const TEXT_RUNS = new Set([
  "text", "emphasis", "strong", "underline", "subscript", "superscript", "delete", "inlineCode", "inlineMath", "break", "abbreviation",
]);

/** A copy of `json` where every node but text runs has the ID `claim` gives it. */
export function claimMystIds(json: Myst, claim: (id: string | null) => string): Myst {
  const copy: Myst = { ...json };
  if (!TEXT_RUNS.has(copy.type)) copy.id = claim(typeof copy.id === "string" ? copy.id : null);
  if (Array.isArray(copy.children)) copy.children = copy.children.map((c: Myst) => claimMystIds(c, claim));
  return copy;
}

/** Every lv-notes ID in a MyST subtree. */
export function mystIds(json: Myst, out: string[] = []): string[] {
  if (typeof json?.id === "string") out.push(json.id);
  for (const child of Array.isArray(json?.children) ? json.children : []) mystIds(child, out);
  return out;
}

/** A copy of `json` with each ID replaced by `rename(id)`. */
export function mapMystIds(json: Myst, rename: (id: string) => string): Myst {
  const copy: Myst = { ...json };
  if (typeof copy.id === "string") copy.id = rename(copy.id);
  if (Array.isArray(copy.children)) copy.children = copy.children.map((c: Myst) => mapMystIds(c, rename));
  return copy;
}
