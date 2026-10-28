//! The commands as the web UI calls them: JSON in, JSON out, through
//! Tauri's IPC (on its mock runtime), with the argument names and shapes
//! `src/backend.ts` uses, under the app's real config and capabilities.

use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{WebviewWindow, WebviewWindowBuilder};

fn invoke(window: &Window, cmd: &str, args: Value) -> Result<Value, Value> {
    get_ipc_response(
        window,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

type Window = WebviewWindow<tauri::test::MockRuntime>;

/// A fresh app with its settings in `config`, and its main window.
fn start(config: &std::path::Path) -> (tauri::App<tauri::test::MockRuntime>, Window) {
    let app = lv_desktop::app_in(mock_builder(), config).build(tauri::generate_context!()).unwrap();
    let window = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
    (app, window)
}

#[test]
fn the_ui_round_trip() {
    let config = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let (app, window) = start(config.path());

    let err = invoke(&window, "project_info", json!({})).unwrap_err();
    assert_eq!(err["code"], "NO_PROJECT");
    assert_eq!(invoke(&window, "open_last_project", json!({})).unwrap(), Value::Null);

    let path = files.path().join("Thesis.lv");
    let info = invoke(&window, "create_project", json!({ "path": path, "name": "Thesis" })).unwrap();
    assert_eq!((info["name"].as_str(), info["documents"].as_array().map(Vec::len)), (Some("Thesis"), Some(0)));

    let created = invoke(&window, "create_document", json!({ "title": "Untitled" })).unwrap();
    let doc = created["doc"].as_str().unwrap().to_owned();
    assert!(doc.starts_with("doc_"));

    let loaded = invoke(&window, "load_document", json!({ "doc": doc })).unwrap();
    let mut myst = loaded["myst"].clone();
    myst["children"][0]["children"] = json!([{ "type": "text", "value": "Hello" }]);
    let saved = invoke(&window, "save_document", json!({ "doc": doc, "base": loaded["version"], "myst": myst })).unwrap();
    assert_eq!(saved["ops"], 1);
    assert_eq!(saved["renamed"], json!([]));

    let stale = invoke(&window, "save_document", json!({ "doc": doc, "base": loaded["version"], "myst": myst })).unwrap_err();
    assert_eq!(stale["code"], "STALE_BASE");

    let info = invoke(&window, "rename_document", json!({ "doc": doc, "title": "Hello" })).unwrap();
    assert_eq!(info["documents"][0]["title"], "Hello");

    // A new app over the same settings reopens the project.
    drop(window);
    drop(app);
    let (_app, window) = start(config.path());
    let info = invoke(&window, "open_last_project", json!({})).unwrap();
    assert_eq!(info["name"], "Thesis");
    let loaded = invoke(&window, "load_document", json!({ "doc": doc })).unwrap();
    assert_eq!(loaded["myst"], myst);

    // Linking: find a block, link to it from another note, see the backlink.
    let hits = invoke(&window, "search_blocks", json!({ "text": "hel" })).unwrap();
    let block = hits[0]["node"].as_str().unwrap().to_owned();
    assert_eq!((hits[0]["doc"].as_str(), hits[0]["text"].as_str()), (Some(doc.as_str()), Some("Hello")));
    let found = invoke(&window, "search", json!({ "text": "hel" })).unwrap();
    assert_eq!((found["blocks"][0]["node"].as_str(), &found["more"]), (Some(block.as_str()), &json!(false)));
    assert!(found["documents"].is_array());
    let located = invoke(&window, "locate", json!({ "target": block })).unwrap();
    assert_eq!(located["doc"], doc);
    assert_eq!(invoke(&window, "locate", json!({ "target": "https://example.org" })).unwrap(), Value::Null);
    let other = invoke(&window, "create_document", json!({ "title": "Other" })).unwrap()["doc"].as_str().unwrap().to_owned();
    let loaded = invoke(&window, "load_document", json!({ "doc": other })).unwrap();
    let mut linking = loaded["myst"].clone();
    linking["children"][0]["children"] = json!([{ "type": "link", "url": block, "children": [{ "type": "text", "value": "hi" }] }]);
    let err = invoke(&window, "save_document", json!({ "doc": other, "base": loaded["version"], "myst": linking })).unwrap_err();
    assert_eq!(err["code"], "INVALID_DOCUMENT", "links need IDs too");
    linking["children"][0]["children"][0]["id"] = json!(lv_core::ids::NodeId::new());
    invoke(&window, "save_document", json!({ "doc": other, "base": loaded["version"], "myst": linking })).unwrap();
    let links = invoke(&window, "backlinks", json!({ "doc": doc })).unwrap();
    assert_eq!((links[0]["source_doc"].as_str(), links[0]["target"].as_str()), (Some(other.as_str()), Some(block.as_str())));
    let registry = invoke(&window, "registry", json!({})).unwrap();
    assert!(registry.as_array().unwrap().iter().any(|e| e["namespace"] == "relation" && e["name"] == "supports" && e["label"] == "Supports"));
    let exported = invoke(&window, "export_project", json!({ "dir": files.path(), "format": "obsidian" })).unwrap();
    assert_eq!(exported["files"], 2);
    let imported = invoke(&window, "import_folder", json!({ "dir": exported["path"], "format": "obsidian" })).unwrap();
    assert_eq!((imported["notes"].as_u64(), imported["info"]["documents"].as_array().map(Vec::len)), (Some(2), Some(4)));
    let book = invoke(&window, "export_project", json!({ "dir": files.path(), "format": "book" })).unwrap();
    assert_eq!(book["files"], 4);
    let one = files.path().join("hello.md");
    invoke(&window, "export_document", json!({ "doc": doc, "path": one })).unwrap();
    assert!(std::fs::read_to_string(&one).unwrap().contains("Hello"));
    invoke(&window, "delete_document", json!({ "doc": other })).unwrap();
    assert_eq!(invoke(&window, "backlinks", json!({ "doc": doc })).unwrap(), json!([]));

    // The imported copies stay.
    let info = invoke(&window, "delete_document", json!({ "doc": doc })).unwrap();
    let titles: Vec<&str> = info["documents"].as_array().unwrap().iter().map(|d| d["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Hello", "Other"]);
}
