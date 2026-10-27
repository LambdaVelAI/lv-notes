// The app: a project sidebar, and one document open in the MyST editor.
//
// Saving: edits are debounced and the whole document goes to the core as
// native MyST with the version it was based on. The core works out the ops,
// so this file never builds ops itself. One save is in flight at a time.

import { type Backend, BackendError, type ProjectInfo, connect } from "./backend";
import { MystEditor } from "./editor/editor";
import type { Myst } from "./editor/myst";
import { commands } from "./editor/plugins";
import { schema } from "./editor/schema";
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

const editor = new MystEditor($("#editor"), { onEdit: edited, onUpdate: refreshToolbar });
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

async function openDocument(doc: string) {
  await flush();
  const loaded = await backend.loadDocument(doc);
  open = { doc, version: loaded.version, dirty: false, saving: null, timer: undefined };
  ui.empty.hidden = true;
  ui.editorPane.hidden = false;
  ui.title.value = loaded.title;
  editor.load(loaded.myst);
  renderDocuments();
  setStatus("Saved");
  editor.focus();
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

// ---- Toolbar -------------------------------------------------------------------

function refreshToolbar() {
  const { $from } = editor.view.state.selection;
  const parent = $from.parent;
  ui.blockType.value =
    parent.type === schema.nodes.heading ? `h${parent.attrs.depth}` : parent.type === schema.nodes.code ? "code" : "p";
  for (const button of ui.toolbar.querySelectorAll<HTMLButtonElement>("button[data-command]")) {
    const command = (commands as any)[button.dataset.command!];
    button.disabled = !command(editor.view.state);
  }
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
  backend.onChange((change) => {
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
  $("#new-document-empty").onclick = () => void newDocument();
  ui.title.onchange = () => void renameOpen();
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
