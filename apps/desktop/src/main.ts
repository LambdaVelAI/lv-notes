// The app: a project sidebar, and one document open in the MyST editor.
//
// Saving: edits are debounced and the whole document goes to the core as
// native MyST with the version it was based on. The core works out the ops,
// so this file never builds ops itself. One save is in flight at a time.

import { type Backend, BackendError, type Backlink, type FolderFormat, type Located, type ProjectInfo, type RegistryEntry, connect } from "./backend";
import { MystEditor } from "./editor/editor";
import { isInternalTarget } from "./editor/links";
import type { Myst } from "./editor/myst";
import { commands } from "./editor/plugins";
import { schema } from "./editor/schema";
import { LinkBubble, LinkPicker, renderBacklinks, shorten } from "./linking";
import "./styles.css";

const SAVE_DELAY_MS = 400;

const $ = <T extends HTMLElement>(selector: string) => document.querySelector(selector) as T;

const ui = {
  welcome: $("#welcome"),
  workspace: $("#workspace"),
  projectName: $("#project-name"),
  documents: $<HTMLUListElement>("#documents"),
  title: $<HTMLInputElement>("#title"),
  status: $("#status"),
  blockType: $<HTMLSelectElement>("#block-type"),
  toolbar: $("#toolbar"),
  empty: $("#no-document"),
  editorPane: $("#editor-pane"),
  backlinks: $("#backlinks"),
  linkButton: $<HTMLButtonElement>("#link-button"),
  importMenu: $<HTMLButtonElement>("#import-menu"),
  exportMenu: $<HTMLButtonElement>("#export-menu"),
};

interface Open {
  doc: string;
  /** The version the editor's content is based on. */
  version: number;
  dirty: boolean;
  saving: Promise<void> | null;
  timer: number | undefined;
}

let backend: Backend;
let project: ProjectInfo | null = null;
let open: Open | null = null;

const editor = new MystEditor($("#editor"), {
  onEdit: edited,
  onUpdate: refreshToolbar,
  onLinkRequest: () => openPicker(),
  onFollowLink: (url) => void follow(url),
});
// For poking at the editor from devtools during development.
if (import.meta.env.DEV) Object.assign(window, { lvEditor: editor });

function setStatus(text: string, kind: "ok" | "busy" | "error" = "ok") {
  ui.status.textContent = text;
  ui.status.dataset.kind = kind;
}

// ---- Project and document list -------------------------------------------------

function showProject(info: ProjectInfo | null) {
  project = info;
  ui.welcome.hidden = !!info;
  ui.workspace.hidden = !info;
  if (!info) return;
  ui.projectName.textContent = info.name;
  ui.projectName.title = info.path;
  renderDocuments();
  void refreshVocabulary();
}

async function exportWith(task: () => Promise<{ path: string; files: number } | null>) {
  await flush();
  try {
    const done = await task();
    if (done) setStatus(`Exported ${done.files} ${done.files === 1 ? "note" : "notes"} to ${done.path}`);
  } catch (e) {
    setStatus(`Not exported: ${e instanceof Error ? e.message : String(e)}`, "error");
  }
}

/** A small menu above `anchor`: each item is a label and what it does. */
function showMenu(anchor: HTMLElement, items: [string, (() => void) | null][]) {
  document.querySelector(".menu")?.remove();
  const menu = document.createElement("div");
  menu.className = "menu";
  for (const [label, action] of items) {
    const item = document.createElement("button");
    item.textContent = label;
    item.disabled = !action;
    item.onclick = () => {
      menu.remove();
      action?.();
    };
    menu.append(item);
  }
  document.body.append(menu);
  const at = anchor.getBoundingClientRect();
  menu.style.left = `${at.left}px`;
  menu.style.bottom = `${window.innerHeight - at.top + 6}px`;
  const close = (e: MouseEvent) => {
    if (!menu.contains(e.target as Node)) {
      menu.remove();
      document.removeEventListener("mousedown", close, true);
    }
  };
  setTimeout(() => document.addEventListener("mousedown", close, true));
}

async function importFolder(format: FolderFormat) {
  await flush();
  try {
    const done = await backend.importFolder(format);
    if (!done) return;
    showProject(done.info);
    const notes = `Imported ${done.notes} ${done.notes === 1 ? "note" : "notes"}`;
    const skipped = done.skipped.length ? `; skipped ${done.skipped.length} (notebooks aren't imported yet)` : "";
    const unresolved = done.unresolved ? `; ${done.unresolved} links point outside the folder` : "";
    if (done.first) await openDocument(done.first);
    setStatus(notes + skipped + unresolved);
  } catch (e) {
    setStatus(`Not imported: ${e instanceof Error ? e.message : String(e)}`, "error");
  }
}

function renderDocuments() {
  ui.documents.replaceChildren(
    ...(project?.documents ?? []).map((d) => {
      const li = document.createElement("li");
      li.className = d.id === open?.doc ? "active" : "";
      const button = document.createElement("button");
      button.className = "doc";
      button.textContent = d.title || "Untitled";
      button.onclick = () => void openDocument(d.id);
      const remove = document.createElement("button");
      remove.className = "remove";
      remove.title = "Delete";
      remove.textContent = "×";
      remove.onclick = () => void deleteDocument(d.id, d.title);
      li.append(button, remove);
      return li;
    }),
  );
}

/** Opens `doc`, or stays on it if open; then shows the node `reveal`, if given. */
async function openDocument(doc: string, reveal?: string) {
  if (open?.doc !== doc) {
    await flush();
    const loaded = await backend.loadDocument(doc);
    open = { doc, version: loaded.version, dirty: false, saving: null, timer: undefined };
    ui.empty.hidden = true;
    ui.editorPane.hidden = false;
    ui.title.value = loaded.title;
    editor.load(loaded.myst);
    renderDocuments();
    setStatus("Saved");
    void refreshBacklinks();
    void checkLinks();
  }
  editor.focus();
  if (reveal) editor.reveal(reveal);
}

async function newDocument() {
  const { doc, info } = await backend.createDocument("Untitled");
  showProject(info);
  await openDocument(doc);
  ui.title.select();
}

async function deleteDocument(doc: string, title: string) {
  if (!confirm(`Delete “${title || "Untitled"}”?`)) return;
  if (open?.doc === doc) {
    await flush();
    open = null;
    ui.editorPane.hidden = true;
    ui.empty.hidden = false;
  }
  showProject(await backend.deleteDocument(doc));
  void refreshBacklinks();
}

// ---- Saving ------------------------------------------------------------------

function edited() {
  if (!open) return;
  open.dirty = true;
  setStatus("Edited", "busy");
  clearTimeout(open.timer);
  open.timer = window.setTimeout(() => void save(), SAVE_DELAY_MS);
}

async function save(): Promise<void> {
  const o = open;
  if (!o) return;
  if (o.saving) {
    // Save again once the one in flight is done, with whatever is new.
    await o.saving;
    return o.dirty ? save() : undefined;
  }
  clearTimeout(o.timer);
  if (!o.dirty) return;
  o.dirty = false;
  const myst: Myst = editor.myst();
  setStatus("Saving…", "busy");
  // Mark the save in flight before calling the backend, so a change event
  // for our own save is never taken for someone else's.
  let settle!: () => void;
  o.saving = new Promise((resolve) => (settle = resolve));
  await (async () => {
    try {
      const saved = await backend.saveDocument(o.doc, o.version, myst);
      o.version = saved.version;
      if (open === o) {
        editor.rename(saved.renamed);
        if (!o.dirty) setStatus(`Saved · v${saved.version}`);
      }
    } catch (e) {
      if (e instanceof BackendError && e.code === "STALE_BASE" && open === o) {
        // Changed elsewhere since our base: take the stored version.
        const loaded = await backend.loadDocument(o.doc);
        o.version = loaded.version;
        o.dirty = false;
        editor.load(loaded.myst, true);
        setStatus("Changed elsewhere; reloaded", "error");
      } else {
        o.dirty = true;
        setStatus(`Not saved: ${e instanceof Error ? e.message : String(e)}`, "error");
        console.error(e);
      }
    } finally {
      o.saving = null;
      settle();
    }
  })();
}

/** Saves anything pending, e.g. before switching documents. */
async function flush() {
  if (open?.saving) await open.saving;
  if (open?.dirty) await save();
}

async function renameOpen() {
  if (!open || !project) return;
  const title = ui.title.value.trim();
  const current = project.documents.find((d) => d.id === open!.doc);
  if (current && current.title !== title) showProject(await backend.renameDocument(open.doc, title));
}

// ---- Links and backlinks ------------------------------------------------------

const picker = new LinkPicker({
  documents: () => project?.documents ?? [],
  searchBlocks: (text) => backend.searchBlocks(text),
});

/** Opens the link picker for the selection, or for the link at the cursor. */
function openPicker() {
  if (!open || picker.isOpen) return;
  const { view } = editor;
  const coords = view.coordsAtPos(view.state.selection.from);
  const link = editor.link();
  const query = editor.selectedText() || (link ? view.state.doc.textBetween(link.from, link.to) : "");
  bubble.hide();
  picker.open(
    coords,
    query.length <= 80 ? query : "",
    (choice) => {
      editor.setLink(choice.url, choice.text);
      locations.delete(choice.url);
    },
    () => editor.focus(),
  );
}

/** Where internal link targets are, cached until the project changes. */
const locations = new Map<string, Promise<Located | null>>();
const locate = (url: string) => {
  if (!locations.has(url)) locations.set(url, backend.locate(url).catch(() => null));
  return locations.get(url)!;
};

const bubble = new LinkBubble({
  open: (url) => void follow(url),
  edit: () => openPicker(),
  remove: () => {
    editor.removeLink();
    editor.focus();
  },
  relate: (relation) => {
    editor.setLinkRelation(relation);
    editor.focus();
  },
  blur: () => setTimeout(refreshBubble, 0),
});

/** The project's vocabulary, for the relation menu and backlink labels. */
let vocabulary: RegistryEntry[] = [];
async function refreshVocabulary() {
  vocabulary = await backend.registry().catch((e) => (console.error(e), [] as RegistryEntry[]));
  bubble.setRelations(vocabulary);
}
const relationLabel = (name: string) =>
  vocabulary.find((e) => e.namespace === "relation" && e.name === name)?.label.toLowerCase() ?? name;

function refreshBubble() {
  if (bubble.hasFocus()) return;
  const link = editor.view.hasFocus() && !picker.isOpen ? editor.link() : null;
  if (!link) return bubble.hide();
  const url: string = link.mark.attrs.url;
  const relation: string | null = link.mark.attrs.extra?.relation ?? null;
  const at = editor.view.coordsAtPos(link.from);
  const internal = isInternalTarget(url);
  bubble.show(url, at, internal, undefined, relation);
  if (internal) {
    void locate(url).then((located) => {
      const now = editor.link();
      if (now?.mark.attrs.url === url) bubble.show(url, editor.view.coordsAtPos(now.from), true, located, relation);
    });
  }
}

/** Opens what a link points at: a note, or a block in its note. */
async function follow(url: string) {
  if (!isInternalTarget(url)) {
    // Only web and mail addresses: never files or app schemes from a document.
    if (/^(https?|mailto):/i.test(url)) await backend.openUrl(url).catch((e) => setStatus(`Couldn't open ${shorten(url, 60)}: ${e}`, "error"));
    else setStatus(`Only web and mail links open from here: ${shorten(url, 60)}`);
    return;
  }
  const located = await locate(url);
  if (!located) {
    setStatus("That link's target was deleted", "error");
    return;
  }
  bubble.hide();
  await openDocument(located.doc, located.block?.node);
}

/** Marks the open note's links whose targets are gone. */
let linkCheck = 0;
async function checkLinks() {
  const o = open;
  const check = ++linkCheck;
  if (!o) return;
  const urls = editor.linkTargets();
  const found = await Promise.all(urls.map(locate));
  if (check !== linkCheck || open !== o) return;
  editor.markBroken(urls.filter((_, i) => !found[i]));
}

let backlinksRequest = 0;
async function refreshBacklinks() {
  const o = open;
  const request = ++backlinksRequest;
  if (!o) return;
  const links = await backend.backlinks(o.doc).catch((e) => (console.error(e), [] as Backlink[]));
  if (request !== backlinksRequest || open !== o) return;
  renderBacklinks(ui.backlinks, links, (link) => void openDocument(link.source_doc, link.source), relationLabel);
}

ui.linkButton.addEventListener("mousedown", (e) => {
  e.preventDefault();
  openPicker();
});
editor.view.dom.addEventListener("blur", () => setTimeout(refreshBubble, 0));
ui.editorPane.addEventListener("scroll", () => bubble.hide());

// ---- Toolbar -------------------------------------------------------------------

function refreshToolbar() {
  // The toolbar acts on the document, so it has nothing to show while the title is being edited.
  const inTitle = document.activeElement === ui.title;
  const { $from } = editor.view.state.selection;
  const parent = $from.parent;
  ui.blockType.disabled = inTitle;
  ui.blockType.value = inTitle
    ? ""
    : parent.type === schema.nodes.heading ? `h${parent.attrs.depth}` : parent.type === schema.nodes.code ? "code" : "p";
  for (const button of ui.toolbar.querySelectorAll<HTMLButtonElement>("button[data-command]")) {
    const command = (commands as any)[button.dataset.command!];
    button.disabled = inTitle || !command(editor.view.state);
  }
  ui.linkButton.disabled = inTitle || !open || !!parent.type.spec.code || !parent.inlineContent;
  refreshBubble();
}

function run(command: (state: any, dispatch?: any) => boolean) {
  command(editor.view.state, editor.view.dispatch);
  editor.focus();
}

ui.blockType.onchange = () => {
  const value = ui.blockType.value;
  run(value === "p" ? commands.paragraph : value === "code" ? commands.code : commands.heading(Number(value.slice(1))));
};
ui.toolbar.addEventListener("mousedown", (e) => {
  const button = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-command]");
  if (!button) return;
  e.preventDefault();
  run((commands as any)[button.dataset.command!]);
});

// ---- Start -----------------------------------------------------------------------

async function start() {
  backend = await connect(async () => {
    // The browser preview opens the core's MyST fixtures; never in builds.
    if (!import.meta.env.DEV) return [];
    const fixtures = import.meta.glob<{ default: Myst }>("../../../crates/lv-core/tests/fixtures/myst/*.json");
    const docs = await Promise.all(
      Object.entries(fixtures).map(async ([path, load]) => ({
        title: path.split("/").pop()!.replace(".json", ""),
        myst: (await load()).default,
      })),
    );
    return docs;
  });
  let backlinksTimer: number | undefined;
  backend.onChange((change) => {
    // Any change can add or remove links to the open note, or move targets.
    locations.clear();
    void refreshVocabulary();
    clearTimeout(backlinksTimer);
    backlinksTimer = window.setTimeout(() => {
      void refreshBacklinks();
      void checkLinks();
    }, 150);
    // Someone else (another window, later the agent) changed the open document.
    const o = open;
    if (!o || !change.documents.includes(o.doc) || change.version <= o.version || o.dirty || o.saving) return;
    void backend.loadDocument(o.doc).then((loaded) => {
      if (open !== o || o.dirty || o.saving) return;
      o.version = loaded.version;
      editor.load(loaded.myst, true);
      setStatus(`Updated · v${loaded.version}`);
    });
  });
  backend.beforeClose?.(flush);

  $("#new-project").onclick = async () => showProject(await backend.createProject());
  $("#open-project").onclick = async () => showProject(await backend.openProject());
  $("#switch-project").onclick = async () => {
    await flush();
    const info = await backend.openProject();
    if (info) {
      open = null;
      ui.editorPane.hidden = true;
      ui.empty.hidden = false;
      showProject(info);
    }
  };
  $("#new-document").onclick = () => void newDocument();
  ui.importMenu.onclick = () =>
    showMenu(ui.importMenu, [
      ["Jupyter Book folder…", () => void importFolder("book")],
      ["Obsidian vault…", () => void importFolder("obsidian")],
    ]);
  ui.exportMenu.onclick = () => {
    const o = open;
    showMenu(ui.exportMenu, [
      ["This note as Markdown…", o ? () => void exportWith(() => backend.exportDocument(o.doc, ui.title.value.trim())) : null],
      ["All notes as a Jupyter Book…", () => void exportWith(() => backend.exportProject("book"))],
      ["All notes as an Obsidian vault…", () => void exportWith(() => backend.exportProject("obsidian"))],
    ]);
  };
  $("#new-document-empty").onclick = () => void newDocument();
  ui.title.onchange = () => void renameOpen();
  ui.title.onfocus = ui.title.onblur = () => refreshToolbar();
  ui.title.onkeydown = (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      editor.focus();
    }
  };

  const info = await backend.openLastProject();
  showProject(info);
  if (info?.documents.length) await openDocument(info.documents[0].id);
}

void start().catch((e) => {
  console.error(e);
  setStatus(String(e), "error");
});
