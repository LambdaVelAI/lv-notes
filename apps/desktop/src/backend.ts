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

/** A link into an open document (`target` null: to the document itself). */
export interface Backlink {
  source: string;
  source_doc: string;
  source_title: string;
  target: string | null;
  relation: string | null;
  /** The text of the block the link sits in. */
  context: string;
}

/** A block, where it lives, and its text. */
export interface BlockInfo {
  node: string;
  doc: string;
  title: string;
  /** MyST type: `paragraph`, `heading`, ... */
  kind: string;
  text: string;
}

/** Where a link target is now; `block` is set for a block target. */
export interface Located {
  doc: string;
  title: string;
  block: BlockInfo | null;
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
  /** Links pointing into `doc`. */
  backlinks(doc: string): Promise<Backlink[]>;
  /** Blocks matching every word of `text`, as prefixes. */
  searchBlocks(text: string): Promise<BlockInfo[]>;
  /** Where a `doc_…` / `node_…` link target is; null if missing. */
  locate(target: string): Promise<Located | null>;
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
    backlinks: (doc) => call("backlinks", { doc }),
    searchBlocks: (text) => call("search_blocks", { text }),
    locate: (target) => call("locate", { target }),
    onChange(listener) {
      void listen<Change>("project-changed", (event) => listener(event.payload));
    },
    beforeClose(task) {
      // The window closes once the handler's promise settles.
      void getCurrentWindow().onCloseRequested(() => task().catch(console.error));
    },
  };
}

const PHRASING = new Set(["text", "emphasis", "strong", "underline", "delete", "subscript", "superscript", "inlineCode", "inlineMath", "break", "abbreviation", "link", "crossReference", "cite", "citeGroup", "footnoteReference", "mystRole"]);
const textOf = (n: Myst): string =>
  typeof n.value === "string" ? n.value : Array.isArray(n.children) ? n.children.map(textOf).join("") : "";

/** Every node in `myst` with its nearest enclosing block (itself for blocks). */
function* walk(myst: Myst, block: Myst | null = null): Generator<[Myst, Myst | null]> {
  const own = PHRASING.has(myst.type) || myst.type === "root" ? block : myst;
  yield [myst, own];
  for (const child of myst.children ?? []) yield* walk(child, own);
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
    // The same answers as the core's queries, worked out from the trees.
    async backlinks(doc) {
      const target = get(doc);
      const inTarget = new Set([...walk(target.myst)].map(([n]) => n.id).filter(Boolean));
      const links: Backlink[] = [];
      for (const [source, d] of docs) {
        for (const [n, block] of walk(d.myst)) {
          if (n.type !== "link" || !(n.url === doc || inTarget.has(n.url))) continue;
          links.push({ source: n.id, source_doc: source, source_title: d.title, target: n.url === doc ? null : n.url, relation: n.relation ?? null, context: block ? textOf(block) : "" });
        }
      }
      return links;
    },
    async searchBlocks(text) {
      const words = text.toLowerCase().split(/\s+/).filter(Boolean);
      if (!words.length) return [];
      const hits: BlockInfo[] = [];
      for (const [doc, d] of docs) {
        for (const [n, block] of walk(d.myst)) {
          if (n !== block || !n.id || n.children?.some((c: Myst) => !PHRASING.has(c.type))) continue;
          const t = textOf(n);
          const tokens = t.toLowerCase().split(/\W+/);
          if (words.every((w) => tokens.some((tok) => tok.startsWith(w)))) hits.push({ node: n.id, doc, title: d.title, kind: n.type, text: t });
        }
      }
      return hits.slice(0, 20);
    },
    async locate(target) {
      const d = docs.get(target);
      if (d) return { doc: target, title: d.title, block: null };
      for (const [doc, d] of docs) {
        for (const [n] of walk(d.myst)) {
          if (n.id === target && n.type !== "root") return { doc, title: d.title, block: { node: target, doc, title: d.title, kind: n.type, text: textOf(n) } };
        }
      }
      return null;
    },
    onChange: (l) => void listeners.push(l),
  };
}

export async function connect(seed: () => Promise<{ title: string; myst: Myst }[]>): Promise<Backend> {
  return "__TAURI_INTERNALS__" in window ? tauriBackend() : memoryBackend(await seed());
}
