// How the UI reaches the core. In the app this is Tauri commands (the
// in-process client protocol); in a plain browser it is an in-memory stand-in
// so the UI can be previewed and developed without the shell.

import type { Myst } from "./editor/myst";

export interface DocumentSummary {
  id: string;
  kind: "note" | "draft";
  title: string;
}

export interface ProjectInfo {
  path: string;
  name: string;
  version: number;
  documents: DocumentSummary[];
}

export interface LoadedDocument {
  doc: string;
  title: string;
  version: number;
  myst: Myst;
}

export interface Saved {
  version: number;
  ops: number;
  renamed: [string, string][];
}

export interface Change {
  version: number;
  documents: string[];
}

/** A failed command: a stable code (refusal codes, `STALE_BASE`, ...) and a message. */
export class BackendError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

export interface Backend {
  /** Asks where to create a project; null if cancelled. */
  createProject(): Promise<ProjectInfo | null>;
  /** Asks which project to open; null if cancelled. */
  openProject(): Promise<ProjectInfo | null>;
  openLastProject(): Promise<ProjectInfo | null>;
  createDocument(title: string): Promise<{ doc: string; info: ProjectInfo }>;
  loadDocument(doc: string): Promise<LoadedDocument>;
  saveDocument(doc: string, base: number, myst: Myst): Promise<Saved>;
  renameDocument(doc: string, title: string): Promise<ProjectInfo>;
  deleteDocument(doc: string): Promise<ProjectInfo>;
  onChange(listener: (change: Change) => void): void;
  /** Runs `task` (e.g. a final save) before the window closes. */
  beforeClose?(task: () => Promise<void>): void;
}

const FILTERS = [{ name: "lv-notes project", extensions: ["lv"] }];

async function tauriBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const dialog = await import("@tauri-apps/plugin-dialog");
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const call = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
    try {
      return await invoke<T>(command, args);
    } catch (e: any) {
      throw new BackendError(e?.code ?? "UNKNOWN", e?.message ?? String(e));
    }
  };
  return {
    async createProject() {
      const path = await dialog.save({ title: "New project", defaultPath: "Untitled.lv", filters: FILTERS });
      if (!path) return null;
      const name = path.split(/[\\/]/).pop()!.replace(/\.lv$/, "");
      return call("create_project", { path, name });
    },
    async openProject() {
      const path = await dialog.open({ title: "Open project", multiple: false, directory: false, filters: FILTERS });
      return path ? call("open_project", { path }) : null;
    },
    openLastProject: () => call("open_last_project"),
    createDocument: (title) => call("create_document", { title }),
    loadDocument: (doc) => call("load_document", { doc }),
    saveDocument: (doc, base, myst) => call("save_document", { doc, base, myst }),
    renameDocument: (doc, title) => call("rename_document", { doc, title }),
    deleteDocument: (doc) => call("delete_document", { doc }),
    onChange(listener) {
      void listen<Change>("project-changed", (event) => listener(event.payload));
    },
    beforeClose(task) {
      // The window closes once the handler's promise settles.
      void getCurrentWindow().onCloseRequested(() => task().catch(console.error));
    },
  };
}

/** Keeps documents as MyST in memory. No ops, no reconcile: UI preview only. */
export function memoryBackend(seed: { title: string; myst: Myst }[] = []): Backend {
  let version = 0;
  const docs = new Map<string, { title: string; myst: Myst }>();
  const listeners: ((c: Change) => void)[] = [];
  const id = () => `doc_${crypto.randomUUID()}`;
  for (const d of seed) docs.set(id(), structuredClone(d));
  const info = (): ProjectInfo => ({
    path: "memory",
    name: "Preview project",
    version,
    documents: [...docs].map(([id, d]) => ({ id, kind: "note", title: d.title })),
  });
  const get = (doc: string) => {
    const d = docs.get(doc);
    if (!d) throw new BackendError("UNKNOWN_DOCUMENT", `${doc} does not exist`);
    return d;
  };
  // Events arrive asynchronously, as Tauri's do.
  const changed = (documents: string[]) => {
    const change = { version, documents };
    setTimeout(() => listeners.forEach((l) => l(change)));
  };
  return {
    createProject: async () => info(),
    openProject: async () => info(),
    openLastProject: async () => info(),
    async createDocument(title) {
      const doc = id();
      docs.set(doc, { title, myst: { type: "root", children: [{ type: "paragraph", children: [] }] } });
      version++;
      changed([doc]);
      return { doc, info: info() };
    },
    async loadDocument(doc) {
      const d = get(doc);
      return { doc, title: d.title, version, myst: structuredClone(d.myst) };
    },
    async saveDocument(doc, _base, myst) {
      get(doc).myst = structuredClone(myst);
      version++;
      changed([doc]);
      return { version, ops: 1, renamed: [] };
    },
    async renameDocument(doc, title) {
      get(doc).title = title;
      version++;
      return info();
    },
    async deleteDocument(doc) {
      docs.delete(doc);
      version++;
      changed([doc]);
      return info();
    },
    onChange: (l) => void listeners.push(l),
  };
}

export async function connect(seed: () => Promise<{ title: string; myst: Myst }[]>): Promise<Backend> {
  return "__TAURI_INTERNALS__" in window ? tauriBackend() : memoryBackend(await seed());
}
