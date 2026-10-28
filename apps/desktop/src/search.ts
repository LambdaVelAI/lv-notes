// The search screen: notes by title and blocks by text, the blocks grouped
// by note, with the matching words marked. Plain DOM, like linking.ts.

import { type SearchResults, wordsOf } from "./backend";
import { KIND_LABELS } from "./linking";

/** A run of text, marked when it is (the start of) a matching word. */
export interface Segment {
  text: string;
  match: boolean;
}

const WORD = /[\p{L}\p{N}]+/gu;

/** Where the words of `text` that start with one of `words` match: [start, end) pairs. */
function matches(text: string, words: string[]): [number, number][] {
  const found: [number, number][] = [];
  for (const m of text.matchAll(WORD)) {
    const token = m[0].toLowerCase();
    const longest = Math.max(0, ...words.filter((w) => token.startsWith(w)).map((w) => w.length));
    // Lowercasing can change lengths (rare); then mark the whole word.
    if (longest) found.push([m.index, m.index + (token.length === m[0].length ? longest : m[0].length)]);
  }
  return found;
}

/** `text` split into runs, marking the matching start of each word that starts with one of `words`. */
export function highlight(text: string, words: string[]): Segment[] {
  const segments: Segment[] = [];
  let at = 0;
  for (const [start, end] of matches(text, words)) {
    if (start > at) segments.push({ text: text.slice(at, start), match: false });
    segments.push({ text: text.slice(start, end), match: true });
    at = end;
  }
  if (at < text.length) segments.push({ text: text.slice(at), match: false });
  return segments;
}

/** At most `max` characters of `text`, starting a little before its first match. */
export function snippet(text: string, words: string[], max = 240): string {
  const t = text.replace(/\s+/g, " ").trim();
  if (t.length <= max) return t;
  const first = matches(t, words)[0]?.[0] ?? 0;
  let start = Math.max(0, first - 60);
  if (start > 0) start = t.indexOf(" ", start) + 1 || start;
  const end = start + max;
  let cut = t.slice(start, end);
  if (end < t.length && cut.lastIndexOf(" ") > 0) cut = cut.slice(0, cut.lastIndexOf(" "));
  return `${start > 0 ? "…" : ""}${cut}${end < t.length ? "…" : ""}`;
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = "") => {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text) e.textContent = text;
  return e;
};

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** Renders results into `root`; `open` shows a note, at a block if given. */
export class SearchScreen {
  private items: HTMLButtonElement[] = [];
  private active = -1;

  constructor(
    private root: HTMLElement,
    private open: (doc: string, node?: string) => void,
  ) {}

  show(query: string, results: SearchResults) {
    const words = wordsOf(query);
    const groups = new Map<string, { title: string; blocks: SearchResults["blocks"] }>();
    for (const b of results.blocks) {
      if (!groups.has(b.doc)) groups.set(b.doc, { title: b.title, blocks: [] });
      groups.get(b.doc)!.blocks.push(b);
    }
    this.items = [];
    const marked = (text: string) =>
      highlight(text, words).map((s) => (s.match ? el("mark", "", s.text) : document.createTextNode(s.text)));
    // Note headings open the note too, but the arrow keys step through hits only.
    const button = (className: string, open: () => void, hit = true) => {
      const b = el("button", className);
      b.onclick = open;
      if (hit) {
        b.onmousemove = () => this.setActive(this.items.indexOf(b));
        this.items.push(b);
      }
      return b;
    };

    const parts: HTMLElement[] = [];
    const found = results.blocks.length || results.documents.length;
    const summary = !found
      ? `Nothing matches “${query}”`
      : [
          results.documents.length ? plural(results.documents.length, "note title", "note titles") : "",
          results.blocks.length ? `${plural(results.blocks.length, "block", "blocks")}${results.more ? "+" : ""} in ${plural(groups.size, "note", "notes")}` : "",
        ]
          .filter(Boolean)
          .join(" · ");
    parts.push(el("p", "search-summary", summary));

    if (results.documents.length) {
      const section = el("section", "search-group");
      section.append(el("h2", "", "Notes"));
      const list = el("ul");
      for (const d of results.documents) {
        const b = button("search-hit search-title", () => this.open(d.id));
        b.append(el("span", "kind", "▤"), el("span", "text"));
        b.lastElementChild!.append(...marked(d.title || "Untitled"));
        const li = el("li");
        li.append(b);
        list.append(li);
      }
      section.append(list);
      parts.push(section);
    }

    for (const [doc, group] of groups) {
      const section = el("section", "search-group");
      const head = button("search-note", () => this.open(doc), false);
      head.textContent = group.title || "Untitled";
      const list = el("ul");
      for (const block of group.blocks) {
        const b = button("search-hit", () => this.open(doc, block.node));
        b.title = `Open in “${group.title || "Untitled"}”`;
        const text = el("span", "text");
        text.append(...marked(snippet(block.text, words) || "(empty)"));
        b.append(el("span", "kind", KIND_LABELS[block.kind] ?? "·"), text);
        const li = el("li");
        li.append(b);
        list.append(li);
      }
      section.append(head, list);
      parts.push(section);
    }

    if (results.more) parts.push(el("p", "search-more", "Showing the best matches. Add words to narrow the search."));
    this.root.replaceChildren(...parts);
    this.root.scrollTop = 0;
    this.setActive(this.items.length ? 0 : -1);
  }

  /** Moves the keyboard selection by `delta` (arrow keys). */
  move(delta: number) {
    const n = this.items.length;
    if (n) this.setActive((this.active + delta + n) % n, true);
  }

  /** Opens the selected result (Return). */
  activate() {
    this.items[this.active]?.click();
  }

  private setActive(i: number, scroll = false) {
    this.items[this.active]?.classList.remove("active");
    this.active = i;
    const item = this.items[i];
    item?.classList.add("active");
    if (scroll) item?.scrollIntoView({ block: "nearest" });
  }
}
