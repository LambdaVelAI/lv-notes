// Link UI: the picker (⌘K or `[[`), the bubble shown on a link, and the
// backlinks panel. Plain DOM; the app wires them to the editor and backend.

import type { Backlink, BlockInfo, DocumentSummary, Located } from "./backend";

/** What a link points at, as the picker offers it. */
export interface LinkChoice {
  url: string;
  /** Text to insert when nothing is selected. */
  text: string;
}

interface Option extends LinkChoice {
  kind: string;
  label: string;
  detail?: string;
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = "") => {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text) e.textContent = text;
  return e;
};

const looksLikeUrl = (s: string) => /^[a-z][a-z0-9+.-]*:\S+$/i.test(s) || /^www\.\S+\.\S+$/i.test(s);

/** A block's text cut to a link label. */
export function shorten(text: string, max = 60): string {
  const t = text.replace(/\s+/g, " ").trim();
  return t.length <= max ? t : `${t.slice(0, max - 1).trimEnd()}…`;
}

const KIND_LABELS: Record<string, string> = { paragraph: "¶", heading: "H", listItem: "•", code: "</>", math: "∑", blockquote: "❝" };

export interface PickerSource {
  documents(): DocumentSummary[];
  searchBlocks(text: string): Promise<BlockInfo[]>;
}

/** The link picker: notes by title, blocks by text, or a web address. */
export class LinkPicker {
  private root = el("div", "link-picker");
  private input = el("input");
  private list = el("ul");
  private options: Option[] = [];
  private active = 0;
  private search = 0;
  private onPick: ((choice: LinkChoice) => void) | null = null;
  private onClose: (() => void) | null = null;

  constructor(private source: PickerSource) {
    this.root.hidden = true;
    this.input.placeholder = "Link to a note, a block, or a web address";
    this.input.spellcheck = false;
    this.root.append(this.input, this.list);
    document.body.append(this.root);
    this.input.oninput = () => this.update();
    this.input.onkeydown = (e) => this.key(e);
    this.input.onblur = () => setTimeout(() => this.root.contains(document.activeElement) || this.close(), 0);
  }

  get isOpen() {
    return !this.root.hidden;
  }

  open(at: { left: number; bottom: number }, query: string, onPick: (choice: LinkChoice) => void, onClose: () => void) {
    this.onPick = onPick;
    this.onClose = onClose;
    this.root.hidden = false;
    const width = Math.min(440, window.innerWidth - 32);
    this.root.style.width = `${width}px`;
    this.root.style.left = `${Math.max(16, Math.min(at.left, window.innerWidth - width - 16))}px`;
    this.root.style.top = `${Math.min(at.bottom + 6, window.innerHeight - 320)}px`;
    this.input.value = query;
    this.input.focus();
    this.input.select();
    this.update();
  }

  close() {
    if (this.root.hidden) return;
    this.root.hidden = true;
    this.search++;
    const onClose = this.onClose;
    this.onPick = this.onClose = null;
    onClose?.();
  }

  private pick(option: Option | undefined) {
    if (!option) return;
    const onPick = this.onPick;
    this.onPick = null;
    this.close();
    onPick?.(option);
  }

  private key(e: KeyboardEvent) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const n = this.options.length;
      if (n) this.setActive((this.active + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
    } else if (e.key === "Enter") {
      e.preventDefault();
      this.pick(this.options[this.active]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      this.close();
    }
  }

  private setActive(i: number) {
    this.active = i;
    [...this.list.children].forEach((li, j) => li.classList.toggle("active", j === i));
    this.list.children[i]?.scrollIntoView({ block: "nearest" });
  }

  private update() {
    const query = this.input.value.trim();
    const q = query.toLowerCase();
    const options: Option[] = [];
    if (looksLikeUrl(query)) {
      const url = /^www\./i.test(query) ? `https://${query}` : query;
      options.push({ kind: "web", label: url, detail: "Web address", url, text: query });
    }
    for (const d of this.source.documents()) {
      const title = d.title || "Untitled";
      if (!q || title.toLowerCase().includes(q)) options.push({ kind: "note", label: title, detail: "Note", url: d.id, text: title });
    }
    this.render(options);
    if (!q || looksLikeUrl(query)) return;
    const search = ++this.search;
    void this.source.searchBlocks(query).then((blocks) => {
      if (search !== this.search) return;
      const more = blocks
        .filter((b) => b.text.trim())
        .map((b): Option => ({ kind: b.kind, label: shorten(b.text, 120), detail: b.title || "Untitled", url: b.node, text: shorten(b.text) }));
      this.render([...options, ...more], true);
    }, console.error);
  }

  private render(options: Option[], keepActive = false) {
    this.options = options;
    this.list.replaceChildren(
      ...options.map((o, i) => {
        const li = el("li", `option-${o.kind === "note" || o.kind === "web" ? o.kind : "block"}`);
        li.append(el("span", "kind", o.kind === "note" ? "▤" : o.kind === "web" ? "↗" : KIND_LABELS[o.kind] ?? "·"), el("span", "label", o.label));
        if (o.detail) li.append(el("span", "detail", o.detail));
        li.onmousedown = (e) => {
          e.preventDefault();
          this.pick(o);
        };
        li.onmousemove = () => this.active !== i && this.setActive(i);
        return li;
      }),
    );
    if (!options.length) this.list.append(el("li", "empty", this.input.value.trim() ? "Nothing matches" : "No notes yet"));
    this.setActive(keepActive ? Math.min(this.active, Math.max(options.length - 1, 0)) : 0);
  }
}

export interface BubbleActions {
  open(url: string): void;
  edit(): void;
  remove(): void;
}

/** A small bar under the link at the cursor: where it goes, Open, Edit, Remove. */
export class LinkBubble {
  private root = el("div", "link-bubble");
  private target = el("span", "target");
  private openButton = el("button", "", "Open");
  private url = "";

  constructor(actions: BubbleActions) {
    this.root.hidden = true;
    const edit = el("button", "", "Edit");
    const remove = el("button", "", "Remove");
    this.root.append(this.target, this.openButton, edit, remove);
    document.body.append(this.root);
    // mousedown, so the editor keeps its selection and focus.
    const on = (button: HTMLElement, f: () => void) =>
      (button.onmousedown = (e) => {
        e.preventDefault();
        f();
      });
    on(this.openButton, () => actions.open(this.url));
    on(edit, () => actions.edit());
    on(remove, () => actions.remove());
  }

  /** Shows the bubble for `url` at `at`; `located` is undefined while unknown. */
  show(url: string, at: { left: number; bottom: number }, internal: boolean, located?: Located | null) {
    this.url = url;
    this.root.hidden = false;
    this.root.style.left = `${Math.max(8, Math.min(at.left, window.innerWidth - 360))}px`;
    this.root.style.top = `${at.bottom + 6}px`;
    this.openButton.hidden = !internal && !/^(https?|mailto):/i.test(url);
    this.root.classList.toggle("broken", internal && located === null);
    this.target.title = url;
    if (!internal) this.target.textContent = url;
    else if (located === undefined) this.target.textContent = "…";
    else if (located === null) this.target.textContent = "Missing: the target was deleted";
    else if (located.block) this.target.textContent = `${located.title || "Untitled"} › ${shorten(located.block.text, 48) || located.block.kind}`;
    else this.target.textContent = located.title || "Untitled";
  }

  hide() {
    this.root.hidden = true;
  }
}

/** Renders the backlinks of the open note into `mount`, grouped by source note. */
export function renderBacklinks(mount: HTMLElement, links: Backlink[], openLink: (link: Backlink) => void) {
  const head = el("h2", "", links.length ? `Linked from ${links.length} ${links.length === 1 ? "place" : "places"}` : "No links to this note yet");
  const groups = new Map<string, Backlink[]>();
  for (const l of links) groups.set(l.source_doc, [...(groups.get(l.source_doc) ?? []), l]);
  const list = el("ul");
  for (const group of groups.values()) {
    const li = el("li");
    li.append(el("div", "source", group[0].source_title || "Untitled"));
    for (const link of group) {
      const item = el("button", "backlink");
      item.append(el("span", "context", link.context.trim() || "(link)"));
      const meta = [link.target ? "to a block" : "", link.relation ?? ""].filter(Boolean).join(" · ");
      if (meta) item.append(el("span", "meta", meta));
      item.onclick = () => openLink(link);
      li.append(item);
    }
    list.append(li);
  }
  mount.replaceChildren(head, list);
  mount.classList.toggle("empty", !links.length);
}
