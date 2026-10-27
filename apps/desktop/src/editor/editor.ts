// The editor component: a ProseMirror view over one MyST document.

import { EditorState, Selection, type Transaction } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import { type Myst, mystToPm, pmToMyst } from "./myst";
import { SYSTEM, assignIds, plugins, renameIds } from "./plugins";

export interface EditorOptions {
  /** Called after each user edit (not after loads or ID bookkeeping). */
  onEdit: () => void;
  /** Called after every transaction, e.g. to refresh a toolbar. */
  onUpdate?: (view: EditorView) => void;
}

export class MystEditor {
  readonly view: EditorView;

  constructor(mount: HTMLElement, private options: EditorOptions) {
    this.view = new EditorView(mount, {
      state: this.stateFor({ type: "root", children: [] }),
      dispatchTransaction: (tr) => this.dispatch(tr),
      attributes: { class: "myst-editor", spellcheck: "true" },
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

  focus() {
    this.view.focus();
  }
}
