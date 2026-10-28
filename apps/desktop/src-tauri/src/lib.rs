//! The lv-notes desktop shell: Tauri commands over a [`Session`].
//!
//! The web UI talks to the core only through these commands, which is the
//! in-process form of the client protocol: the editor sends whole native
//! MyST trees with the version they were based on, and the core turns them
//! into ops. After every accepted change the app emits `project-changed`
//! with the new version and the documents it touched.

mod session;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lv_core::ids::{DocumentId, Version};
use lv_core::op::DocKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

pub use session::{
    BacklinkInfo, BlockInfo, Change, DocumentSummary, Error, Exported, Identity, LoadedDocument, Located, ProjectInfo, Saved, Session,
};

/// Per-user settings, kept as JSON in the app's config directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Settings {
    identity: Identity,
    #[serde(default)]
    last_project: Option<PathBuf>,
}

impl Settings {
    fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| {
            let name = std::env::var("USER").unwrap_or_else(|_| "Me".into());
            Settings { identity: Identity::new(name), last_project: None }
        })
    }

    fn save(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(self).expect("serializes"));
    }
}

struct App {
    session: Mutex<Option<Session>>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
}

impl App {
    fn remember(&self, path: &Path) {
        let mut settings = self.settings.lock().expect("settings lock");
        settings.last_project = Some(path.to_owned());
        settings.save(&self.settings_path);
    }

    fn identity(&self) -> Identity {
        self.settings.lock().expect("settings lock").identity.clone()
    }

    /// Runs `f` on the open session.
    fn with<T>(&self, f: impl FnOnce(&mut Session) -> session::Result<T>) -> Result<T, CommandError> {
        let mut guard = self.session.lock().expect("session lock");
        let session = guard.as_mut().ok_or_else(|| CommandError::new("NO_PROJECT", "no project is open"))?;
        f(session).map_err(CommandError::from)
    }
}

/// What the UI receives when a command fails.
#[derive(Debug, Serialize)]
struct CommandError {
    code: String,
    message: String,
}

impl CommandError {
    fn new(code: &str, message: impl Into<String>) -> CommandError {
        CommandError { code: code.into(), message: message.into() }
    }
}

impl From<Error> for CommandError {
    fn from(e: Error) -> CommandError {
        CommandError { code: e.code(), message: e.to_string() }
    }
}

type Reply<T> = Result<T, CommandError>;

fn changed<R: Runtime>(app: &AppHandle<R>, change: &Change) {
    let _ = app.emit("project-changed", change);
}

fn start(state: &App, session: Session) -> Reply<ProjectInfo> {
    let info = session.info().map_err(CommandError::from)?;
    state.remember(session.path());
    *state.session.lock().expect("session lock") = Some(session);
    Ok(info)
}

#[tauri::command]
fn create_project(state: State<App>, path: PathBuf, name: String) -> Reply<ProjectInfo> {
    let session = Session::create(&path, &name, &state.identity())?;
    start(&state, session)
}

#[tauri::command]
fn open_project(state: State<App>, path: PathBuf) -> Reply<ProjectInfo> {
    let session = Session::open(&path, &state.identity())?;
    start(&state, session)
}

/// Reopens the last project, if there is one and it still opens.
#[tauri::command]
fn open_last_project(state: State<App>) -> Option<ProjectInfo> {
    let path = state.settings.lock().expect("settings lock").last_project.clone()?;
    let session = Session::open(&path, &state.identity()).ok()?;
    start(&state, session).ok()
}

#[tauri::command]
fn project_info(state: State<App>) -> Reply<ProjectInfo> {
    state.with(|s| s.info())
}

#[derive(Serialize)]
struct Created {
    doc: DocumentId,
    info: ProjectInfo,
}

#[tauri::command]
fn create_document<R: Runtime>(app: AppHandle<R>, state: State<App>, title: String) -> Reply<Created> {
    state.with(|s| {
        let (doc, change) = s.create_document(&title, DocKind::Note)?;
        changed(&app, &change);
        Ok(Created { doc, info: s.info()? })
    })
}

#[tauri::command]
fn load_document(state: State<App>, doc: DocumentId) -> Reply<LoadedDocument> {
    state.with(|s| s.load_document(doc))
}

#[tauri::command]
fn save_document<R: Runtime>(app: AppHandle<R>, state: State<App>, doc: DocumentId, base: Version, myst: Value) -> Reply<Saved> {
    state.with(|s| {
        let (saved, change) = s.save_document(doc, base, myst)?;
        if let Some(change) = change {
            changed(&app, &change);
        }
        Ok(saved)
    })
}

#[tauri::command]
fn rename_document<R: Runtime>(app: AppHandle<R>, state: State<App>, doc: DocumentId, title: String) -> Reply<ProjectInfo> {
    state.with(|s| {
        changed(&app, &s.rename_document(doc, &title)?);
        s.info()
    })
}

#[tauri::command]
fn delete_document<R: Runtime>(app: AppHandle<R>, state: State<App>, doc: DocumentId) -> Reply<ProjectInfo> {
    state.with(|s| {
        changed(&app, &s.delete_document(doc)?);
        s.info()
    })
}

/// Links pointing into `doc`, for its backlinks panel.
#[tauri::command]
fn backlinks(state: State<App>, doc: DocumentId) -> Reply<Vec<BacklinkInfo>> {
    state.with(|s| s.backlinks(doc))
}

/// Blocks matching `text`, for the link picker.
#[tauri::command]
fn search_blocks(state: State<App>, text: String, limit: Option<usize>) -> Reply<Vec<BlockInfo>> {
    state.with(|s| s.search_blocks(&text, limit.unwrap_or(20)))
}

/// Where a link target (`doc_…`, `node_…`) is now, or null.
#[tauri::command]
fn locate(state: State<App>, target: String) -> Reply<Option<Located>> {
    state.with(|s| Ok(s.locate(&target)))
}

/// Writes every note as Markdown into a new folder inside `dir`.
#[tauri::command]
fn export_project(state: State<App>, dir: PathBuf) -> Reply<Exported> {
    state.with(|s| s.export_project(&dir))
}

/// Writes one note as Markdown to `path`.
#[tauri::command]
fn export_document(state: State<App>, doc: DocumentId, path: PathBuf) -> Reply<Exported> {
    state.with(|s| s.export_document(doc, &path))
}

impl App {
    fn new(config_dir: &Path) -> App {
        let settings_path = config_dir.join("settings.json");
        let settings = Settings::load(&settings_path);
        settings.save(&settings_path);
        App { session: Mutex::new(None), settings: Mutex::new(settings), settings_path }
    }
}

fn commands<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(tauri_plugin_dialog::init()).plugin(tauri_plugin_opener::init()).invoke_handler(tauri::generate_handler![
        create_project,
        open_project,
        open_last_project,
        project_info,
        create_document,
        load_document,
        save_document,
        rename_document,
        delete_document,
        backlinks,
        search_blocks,
        locate,
        export_project,
        export_document,
    ])
}

/// The app with its settings in the platform's config directory.
pub fn app<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    commands(builder).setup(|app| {
        let dir = app.path().app_config_dir()?;
        app.manage(App::new(&dir));
        Ok(())
    })
}

/// The app with its settings in `config_dir`, for tests.
pub fn app_in<R: Runtime>(builder: tauri::Builder<R>, config_dir: &Path) -> tauri::Builder<R> {
    commands(builder).manage(App::new(config_dir))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    app(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("error while running lv-notes");
}
