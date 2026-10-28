// The editor component: a ProseMirror view over one MyST document.

import { EditorState, NodeSelection, Selection, TextSelection, type Transaction } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import { type Myst, mystToPm, pmToMyst } from "./myst";
import { FLASH, LINK_REQUEST, type LinkRange, findId, linkAt, removeLink, setLink } from "./links";
import { SYSTEM, assignIds, plugins, renameIds } from "./plugins";

export interface EditorOptions {
  /** Called after each user edit (not after loads or ID bookkeeping). */
  onEdit: () => void;
  /** Called after every transaction, e.g. to refresh a toolbar. */
  onUpdate?: (view: EditorView) => void;
  /** The user asked to add or edit a link (⌘K, `[[`). */
  onLinkRequest?: () => void;
  /** The user ⌘-clicked a link. */
  onFollowLink?: (url: string) => void;
}

export class MystEditor {
  readonly view: EditorView;
  private flashTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(mount: HTMLElement, private options: EditorOptions) {
    this.view = new EditorView(mount, {
      state: this.stateFor({ type: "root", children: [] }),
      dispatchTransaction: (tr) => this.dispatch(tr),
      attributes: { class: "myst-editor", spellcheck: "true" },
      handleClick: (view, pos, event) => {
        if (!(event.metaKey || event.ctrlKey) || !this.options.onFollowLink) return false;
        const link = view.state.doc.resolve(pos).marks().find((m) => m.type === view.state.schema.marks.link);
        const url = link?.attrs.url ?? (event.target as HTMLElement).closest?.("a")?.getAttribute("href");
        if (!url) return false;
        this.options.onFollowLink(url);
        return true;
      },
    });
  }

  private stateFor(myst: Myst): EditorState {
    const state = EditorState.create({ doc: mystToPm(myst), plugins: plugins() });
    // Loaded MyST may lack IDs (e.g. a new empty paragraph); give them now.
    const tr = assignIds(state);
    return tr ? state.apply(tr) : state;
  }

  private dispatch(tr: Transaction) {
    const { state, transactions } = this.view.state.applyTransaction(tr);
    this.view.updateState(state);
    if (transactions.some((t) => t.docChanged && !t.getMeta(SYSTEM))) this.options.onEdit();
    this.options.onUpdate?.(this.view);
    // After the key or input event is done, so the picker can take focus.
    if (tr.getMeta(LINK_REQUEST) || transactions.some((t) => t.getMeta(LINK_REQUEST))) {
      setTimeout(() => this.options.onLinkRequest?.());
    }
  }

  /** Replaces the document; `keepCursor` puts the cursor near where it was. */
  load(myst: Myst, keepCursor = false) {
    const head = this.view.state.selection.head;
    let state = this.stateFor(myst);
    if (keepCursor) {
      const $pos = state.doc.resolve(Math.min(head, state.doc.content.size));
      state = state.apply(state.tr.setSelection(Selection.near($pos)).setMeta("addToHistory", false));
    }
    this.view.updateState(state);
    this.options.onUpdate?.(this.view);
  }

  /** The document as native MyST, ready to save. */
  myst(): Myst {
    return pmToMyst(this.view.state.doc);
  }

  /** Applies IDs the core renamed on save. */
  rename(renamed: [string, string][]) {
    const tr = renameIds(this.view.state, renamed);
    if (tr) this.view.dispatch(tr);
  }

  /** The link the cursor or selection is in. */
  link(): LinkRange | null {
    return linkAt(this.view.state);
  }

  /** Links the selection (or the link at the cursor) to `url`; see `setLink`. */
  setLink(url: string, text?: string): boolean {
    return setLink(url, text)(this.view.state, this.view.dispatch);
  }

  removeLink(): boolean {
    return removeLink(this.view.state, this.view.dispatch);
  }

  /** The selected text, e.g. to seed the link picker's query. */
  selectedText(): string {
    const { from, to } = this.view.state.selection;
    return this.view.state.doc.textBetween(from, to, " ");
  }

  /**
   * Scrolls to the block or link with this ID and selects it. Returns false
   * if this document has no such node.
   */
  reveal(id: string): boolean {
    const found = findId(this.view.state.doc, id);
    if (!found) return false;
    const { doc } = this.view.state;
    const selection = found.to != null
      ? TextSelection.create(doc, found.pos, found.to)
      : found.node!.inlineContent
        ? TextSelection.create(doc, found.pos + 1)
        : found.node!.isAtom
          ? NodeSelection.create(doc, found.pos)
          : Selection.near(doc.resolve(found.pos + 1));
    const flash = found.to != null
      ? { from: found.pos, to: found.to, block: false }
      : { from: found.pos, to: found.pos + found.node!.nodeSize, block: true };
    this.view.dispatch(this.view.state.tr.setSelection(selection).setMeta(FLASH, flash).scrollIntoView());
    clearTimeout(this.flashTimer);
    this.flashTimer = setTimeout(() => this.view.dispatch(this.view.state.tr.setMeta(FLASH, null)), 1600);
    return true;
  }

  focus() {
    this.view.focus();
  }
}
