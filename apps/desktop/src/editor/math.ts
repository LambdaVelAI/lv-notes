// Math: MyST `inlineMath` and `math`, shown rendered with KaTeX. The LaTeX
// source is what is stored and what ops carry; rendering is presentation.
//
// - Inline math is an atom. Selecting it (click, or arrowing onto it) opens
//   a small source editor under it; the rendering updates as you type.
// - A display equation is a code-like block whose text is its source. The
//   source shows while the cursor is in it, under the rendering; otherwise
//   only the rendering shows. Equations are numbered in document order (a
//   CSS counter), and can carry a MyST label for references.

import katex from "katex";
import "katex/dist/katex.min.css";
import { type Node as PMNode } from "prosemirror-model";
import { type Command, type EditorState, NodeSelection, Plugin, PluginKey, TextSelection } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView, type NodeView } from "prosemirror-view";
import { schema } from "./schema";

const n = schema.nodes;

/** Renders `latex` into `el`; on a LaTeX error, shows the source and returns the message. */
export function renderMath(el: HTMLElement, latex: string, display: boolean): string | null {
  el.classList.toggle("math-empty", !latex.trim());
  if (!latex.trim()) {
    el.textContent = display ? "Empty equation: type LaTeX below" : "math";
    el.classList.remove("math-error");
    return null;
  }
  try {
    katex.render(latex, el, { displayMode: display, throwOnError: true, strict: "ignore", trust: false });
    el.classList.remove("math-error");
    return null;
  } catch (e) {
    el.textContent = latex;
    el.classList.add("math-error");
    return e instanceof Error ? e.message.replace(/^KaTeX parse error: /, "") : String(e);
  }
}

/** MyST's identifier for a label (lv-core's `markdown::identifier`). */
export const labelIdentifier = (label: string) => label.split(/\s+/).filter(Boolean).join(" ").toLowerCase();

/** `extra` with `label` (and its identifier) set, or removed when empty. */
export function withLabel(extra: Record<string, any> | null, label: string): Record<string, any> | null {
  const { label: _l, identifier: _i, ...rest } = extra ?? {};
  const trimmed = label.trim();
  const next = trimmed ? { ...rest, label: trimmed, identifier: labelIdentifier(trimmed) } : rest;
  return Object.keys(next).length ? next : null;
}

// ---- Where the cursor came from ---------------------------------------------

/** Remembers the previous selection head, so inline math opens at the side it was entered from. */
const lastHead = new PluginKey<number>("math-last-head");

// ---- Inline math -------------------------------------------------------------

class InlineMathView implements NodeView {
  dom: HTMLElement;
  private latex: string | null = null;
  private selected = false;
  private popup: HTMLElement | null = null;
  private input: HTMLTextAreaElement | null = null;
  private message: HTMLElement | null = null;

  constructor(
    private node: PMNode,
    private view: EditorView,
    private getPos: () => number | undefined,
  ) {
    this.dom = document.createElement("span");
    this.dom.className = "math-inline";
    // An atom: the caret must never go inside KaTeX's markup.
    this.dom.contentEditable = "false";
    this.render();
  }

  update(node: PMNode) {
    if (node.type !== this.node.type) return false;
    this.node = node;
    this.render();
    return true;
  }

  private render() {
    const latex = this.node.attrs.value as string;
    if (latex === this.latex) return;
    this.latex = latex;
    const error = renderMath(this.dom, latex, false);
    this.dom.title = error ?? "";
    if (this.message) this.message.textContent = error ?? "";
  }

  selectNode() {
    this.selected = true;
    this.dom.classList.add("ProseMirror-selectednode");
    this.open();
  }

  // Editing the source replaces the node and may reselect it: close only if
  // it stays deselected, so typing in the box isn't interrupted.
  deselectNode() {
    this.selected = false;
    this.dom.classList.remove("ProseMirror-selectednode");
    setTimeout(() => this.selected || this.close());
  }

  stopEvent() {
    return false;
  }

  ignoreMutation() {
    return true;
  }

  destroy() {
    this.close();
  }

  private open() {
    if (this.popup) return;
    const popup = document.createElement("div");
    popup.className = "math-popup";
    const input = document.createElement("textarea");
    input.rows = 1;
    input.spellcheck = false;
    input.placeholder = "LaTeX, e.g. x^2";
    input.value = this.node.attrs.value;
    const message = document.createElement("div");
    message.className = "math-message";
    message.textContent = this.dom.title;
    popup.append(input, message);
    document.body.append(popup);
    this.popup = popup;
    this.input = input;
    this.message = message;
    this.place();

    input.oninput = () => this.edit(input.value);
    input.onkeydown = (e) => this.key(e);
    input.onblur = () => setTimeout(() => this.popup && document.activeElement !== this.input && this.close(), 0);
    const pos = this.getPos();
    const from = lastHead.getState(this.view.state);
    // Arrowing in from the left starts at the beginning; anything else at the end.
    const atStart = pos != null && from === pos;
    setTimeout(() => {
      input.focus();
      const at = atStart ? 0 : input.value.length;
      input.setSelectionRange(at, at);
    });
  }

  private close() {
    this.popup?.remove();
    this.popup = this.input = this.message = null;
  }

  private place() {
    if (!this.popup) return;
    const at = this.dom.getBoundingClientRect();
    const width = Math.min(420, window.innerWidth - 32);
    this.popup.style.width = `${width}px`;
    this.popup.style.left = `${Math.max(16, Math.min(at.left, window.innerWidth - width - 16))}px`;
    this.popup.style.top = `${at.bottom + 6}px`;
  }

  /** Writes the source into the document, keeping this node selected. */
  private edit(value: string) {
    const pos = this.getPos();
    if (pos == null) return;
    const tr = this.view.state.tr.setNodeMarkup(pos, null, { ...this.node.attrs, value });
    this.view.dispatch(tr.setSelection(NodeSelection.create(tr.doc, pos)));
    this.place();
  }

  /** Puts the cursor before or after this node and returns to the text; empty math is removed. */
  private leave(after: boolean) {
    const pos = this.getPos();
    if (pos == null) return;
    const { state } = this.view;
    const tr = state.tr;
    if (!(this.node.attrs.value as string).trim()) tr.delete(pos, pos + this.node.nodeSize);
    else tr.setSelection(TextSelection.create(tr.doc, after ? pos + this.node.nodeSize : pos));
    this.close();
    this.view.dispatch(tr.scrollIntoView());
    this.view.focus();
  }

  private key(e: KeyboardEvent) {
    const input = this.input!;
    const collapsed = input.selectionStart === input.selectionEnd;
    if ((e.key === "Enter" && !e.shiftKey) || e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      this.leave(!(e.key === "Tab" && e.shiftKey));
    } else if (e.key === "ArrowLeft" && collapsed && input.selectionStart === 0) {
      e.preventDefault();
      this.leave(false);
    } else if (e.key === "ArrowRight" && collapsed && input.selectionEnd === input.value.length) {
      e.preventDefault();
      this.leave(true);
    } else if (e.key === "Backspace" && !input.value) {
      e.preventDefault();
      this.leave(false);
    } else if ((e.metaKey || e.ctrlKey) && (e.key === "z" || e.key === "y")) {
      // Undo belongs to the document, not the text box.
      e.preventDefault();
      this.close();
      this.view.focus();
    }
  }
}

// ---- Display math --------------------------------------------------------------

class MathBlockView implements NodeView {
  dom: HTMLElement;
  contentDOM: HTMLElement;
  private rendered: HTMLElement;
  private footer: HTMLElement;
  private message: HTMLElement;
  private label: HTMLInputElement;
  private latex: string | null = null;

  constructor(
    private node: PMNode,
    private view: EditorView,
    private getPos: () => number | undefined,
  ) {
    this.dom = document.createElement("div");
    this.dom.className = "math-display";
    this.rendered = document.createElement("div");
    this.rendered.className = "math-render";
    this.rendered.contentEditable = "false";
    this.rendered.onmousedown = (e) => {
      e.preventDefault();
      this.editSource();
    };
    const number = document.createElement("span");
    number.className = "math-number";
    number.contentEditable = "false";
    this.contentDOM = document.createElement("pre");
    this.contentDOM.className = "math-source";
    this.contentDOM.spellcheck = false;
    this.footer = document.createElement("div");
    this.footer.className = "math-footer";
    this.footer.contentEditable = "false";
    this.message = document.createElement("span");
    this.message.className = "math-message";
    this.label = document.createElement("input");
    this.label.className = "math-label";
    this.label.placeholder = "Label";
    this.label.title = "A name for this equation, for references from MyST ({eq}`label`)";
    this.label.spellcheck = false;
    this.label.oninput = () => this.setLabel(this.label.value);
    this.label.onkeydown = (e) => {
      if (e.key === "Enter" || e.key === "Escape") {
        e.preventDefault();
        this.editSource();
      }
    };
    this.footer.append(this.message, this.label);
    this.dom.append(this.rendered, number, this.contentDOM, this.footer);
    this.sync();
  }

  update(node: PMNode) {
    if (node.type !== this.node.type) return false;
    this.node = node;
    this.sync();
    return true;
  }

  private sync() {
    const { id, extra } = this.node.attrs;
    if (id) this.dom.dataset.id = id;
    this.dom.dataset.enumerated = String(extra?.enumerated !== false);
    if (document.activeElement !== this.label) this.label.value = extra?.label ?? "";
    this.dom.classList.toggle("math-labelled", !!extra?.label);
    const latex = this.node.textContent;
    if (latex === this.latex) return;
    this.latex = latex;
    this.message.textContent = renderMath(this.rendered, latex, true) ?? "";
  }

  private editSource() {
    const pos = this.getPos();
    if (pos == null) return;
    const end = pos + 1 + this.node.content.size;
    this.view.dispatch(this.view.state.tr.setSelection(TextSelection.create(this.view.state.doc, end)));
    this.view.focus();
  }

  private setLabel(label: string) {
    const pos = this.getPos();
    if (pos == null) return;
    const extra = withLabel(this.node.attrs.extra, label);
    this.view.dispatch(this.view.state.tr.setNodeMarkup(pos, null, { ...this.node.attrs, extra }));
  }

  stopEvent(event: Event) {
    return this.footer.contains(event.target as Node) || this.rendered.contains(event.target as Node);
  }

  ignoreMutation(mutation: MutationRecord | { type: "selection"; target: Node }) {
    return !this.contentDOM.contains(mutation.target as Node);
  }
}

export const mathViews = {
  inlineMath: (node: PMNode, view: EditorView, getPos: () => number | undefined) => new InlineMathView(node, view, getPos),
  math: (node: PMNode, view: EditorView, getPos: () => number | undefined) => new MathBlockView(node, view, getPos),
};

/** Marks the display equation the cursor is in, which shows its source. */
export const mathPlugin = new Plugin({
  key: lastHead,
  state: {
    init: () => 0,
    apply: (_tr, _value, old: EditorState) => old.selection.head,
  },
  props: {
    decorations(state) {
      const { $head } = state.selection;
      for (let d = $head.depth; d > 0; d--) {
        if ($head.node(d).type === n.math) {
          return DecorationSet.create(state.doc, [Decoration.node($head.before(d), $head.after(d), { class: "math-editing" })]);
        }
      }
      return null;
    },
  },
});

// ---- Commands ------------------------------------------------------------------

/**
 * Inline math from the selected text, or an empty one at the cursor, opened
 * for editing. Inside a display equation or code, does nothing.
 */
export const insertInlineMath: Command = (state, dispatch) => {
  const { $from, $to, from, to } = state.selection;
  if (!$from.sameParent($to) || !$from.parent.inlineContent || $from.parent.type.spec.code) return false;
  const value = state.doc.textBetween(from, to, " ");
  if (!$from.parent.canReplaceWith($from.index(), $to.index(), n.inlineMath)) return false;
  if (dispatch) {
    const tr = state.tr.replaceRangeWith(from, to, n.inlineMath.create({ value }, null, $from.marks()));
    dispatch(tr.setSelection(NodeSelection.create(tr.doc, from)).scrollIntoView());
  }
  return true;
};

/**
 * Moves the cursor into a display equation next to it, whose source is
 * hidden until the cursor is inside (so the browser would skip it).
 */
export function enterMath(dir: "up" | "down" | "left" | "right"): Command {
  return (state, dispatch, view) => {
    const { $head, empty } = state.selection;
    if (!empty || $head.depth === 0 || !view?.endOfTextblock(dir)) return false;
    const forward = dir === "down" || dir === "right";
    const $edge = state.doc.resolve(forward ? $head.after() : $head.before());
    const next = forward ? $edge.nodeAfter : $edge.nodeBefore;
    if (next?.type !== n.math) return false;
    const pos = forward ? $edge.pos + 1 : $edge.pos - 1;
    dispatch?.(state.tr.setSelection(TextSelection.create(state.doc, pos)).scrollIntoView());
    return true;
  };
}

/** `$x^2$` typed in text becomes inline math. The closing `$` triggers it;
 *  the source must not start or end with a space, so prices stay text. */
export const INLINE_MATH_RULE = /(^|[^$\\])\$([^\s$](?:[^$]*[^\s$\\])?)\$$/;
