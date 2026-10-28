//! Markdown export: a project's documents as MyST Markdown files.
//!
//! Each live document becomes one `.md` file with its title in the
//! frontmatter. Links inside the project are rewritten from IDs to paths: a
//! link to a document (`doc_…`) points at that document's file, and a link
//! to a block (`node_…`) at `file.md#label`. A linked block without a label
//! of its own gets an explicit MyST target, `(node-…)=`, so the anchor
//! resolves. Other lv-notes fields (IDs, relations) are not exported; see
//! `docs/design/ops-and-ids.md`.
//!
//! Nothing here touches the file system: callers write the files.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::convert::{Flavor, enclosing_block, materialize};
use crate::ids::{DocumentId, NodeId};
use crate::state::ProjectState;

/// One exported document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedFile {
    pub doc: DocumentId,
    /// The file name, unique within the export (`reading-notes.md`).
    pub name: String,
    pub markdown: String,
}

/// Every live document as Markdown, oldest first, with links between them
/// rewritten to relative paths.
pub fn project_markdown(state: &ProjectState) -> Vec<ExportedFile> {
    let exporter = Exporter::new(state);
    exporter.docs.iter().map(|&doc| exporter.file(doc)).collect()
}

/// One document as Markdown. Links to other documents point at the file
/// names a whole-project export would give them.
pub fn document_markdown(state: &ProjectState, doc: DocumentId) -> Option<ExportedFile> {
    let exporter = Exporter::new(state);
    exporter.names.contains_key(&doc).then(|| exporter.file(doc))
}

/// A file name for `title`: lowercase words joined by hyphens.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "untitled".to_owned() } else { out.chars().take(80).collect() }
}

struct Exporter<'a> {
    state: &'a ProjectState,
    docs: Vec<DocumentId>,
    names: HashMap<DocumentId, String>,
    /// Labels for linked blocks, and whether the export must add a target.
    anchors: HashMap<NodeId, (String, bool)>,
}

impl<'a> Exporter<'a> {
    fn new(state: &'a ProjectState) -> Exporter<'a> {
        let mut docs: Vec<_> = state.documents().filter(|(_, d)| !d.deleted).map(|(id, d)| (d.created, id)).collect();
        docs.sort();
        let docs: Vec<DocumentId> = docs.into_iter().map(|(_, id)| id).collect();

        let mut taken = HashSet::new();
        let mut names = HashMap::new();
        for &doc in &docs {
            let base = slug(&state.document(doc).expect("live").title);
            let mut name = format!("{base}.md");
            let mut n = 2;
            while !taken.insert(name.clone()) {
                name = format!("{base}-{n}.md");
                n += 1;
            }
            names.insert(doc, name);
        }

        // Every block some live link points at gets a label to link to.
        let mut anchors = HashMap::new();
        for (_, n) in state.nodes() {
            if n.deleted || n.content.node_type != "link" || !names.contains_key(&n.document) {
                continue;
            }
            let Some(target) = n.content.fields.get("url").and_then(Value::as_str).and_then(|u| u.parse::<NodeId>().ok()) else {
                continue;
            };
            let Some(block) = enclosing_block(state, target) else { continue };
            let Some(b) = state.node(block).filter(|b| !b.deleted && names.contains_key(&b.document)) else { continue };
            let label = b.content.fields.get("identifier").and_then(Value::as_str).filter(|_| b.content.fields.contains_key("label"));
            let anchor = match label {
                Some(label) => (label.to_owned(), false),
                None => (format!("node-{}", block.to_string().trim_start_matches("node_")), true),
            };
            anchors.insert(target, anchor.clone());
            anchors.insert(block, anchor);
        }
        Exporter { state, docs, names, anchors }
    }

    fn file(&self, doc: DocumentId) -> ExportedFile {
        let root = materialize(self.state, doc, Flavor::Native).expect("live documents materialize");
        let root = serde_json::to_value(root).expect("serializes");
        let title = &self.state.document(doc).expect("live").title;
        let links = |url: &str| self.resolve(doc, url);
        let targets = |id: &str| {
            let id: NodeId = id.parse().ok()?;
            self.anchors.get(&id).filter(|(_, add)| *add).map(|(label, _)| label.clone())
        };
        let markdown = Writer { links: &links, targets: &targets }.document(&root, title);
        ExportedFile { doc, name: self.names[&doc].clone(), markdown }
    }

    /// A link url as it should read in `from`'s file.
    fn resolve(&self, from: DocumentId, url: &str) -> Option<String> {
        if let Ok(doc) = url.parse::<DocumentId>() {
            return self.names.get(&doc).cloned();
        }
        let node: NodeId = url.parse().ok()?;
        let n = self.state.node(node).filter(|n| !n.deleted)?;
        let (label, _) = self.anchors.get(&node)?;
        Some(if n.document == from { format!("#{label}") } else { format!("{}#{label}", self.names.get(&n.document)?) })
    }
}

// ---- The Markdown writer ----------------------------------------------------

/// Writes MyST Markdown from a MyST AST (as JSON).
pub struct Writer<'a> {
    /// The url to write for a link's url; `None` keeps it as is.
    pub links: &'a dyn Fn(&str) -> Option<String>,
    /// A target label to add before the block with this `id`, if any.
    pub targets: &'a dyn Fn(&str) -> Option<String>,
}

fn s<'v>(node: &'v Value, key: &str) -> Option<&'v str> {
    node.get(key).and_then(Value::as_str)
}

fn kids(node: &Value) -> &[Value] {
    node.get("children").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn ty(node: &Value) -> &str {
    s(node, "type").unwrap_or("")
}

/// Plain text of a node, for role bodies and fallbacks.
fn text(node: &Value) -> String {
    match s(node, "value") {
        Some(v) => v.to_owned(),
        None => kids(node).iter().map(text).collect(),
    }
}

/// Indents `body` with `first` on its first line and `rest` on the others;
/// blank lines get no trailing spaces.
fn indent(body: &str, first: &str, rest: &str) -> String {
    let mut out = String::new();
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let prefix = if i == 0 { first } else { rest };
        if line.is_empty() { out.push_str(prefix.trim_end()) } else { write!(out, "{prefix}{line}").unwrap() }
    }
    out
}

/// A code fence longer than any run of backticks in `body`.
fn fence(body: &str) -> String {
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

/// Escapes characters that would read as Markdown syntax.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let html = c == '<' && chars.peek().is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?'));
        if html || matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escapes what would start a block construct at the start of a line.
fn escape_line_start(line: &str) -> String {
    let t = line.trim_start();
    let marker = |c: char| t.starts_with(c) && (t.len() == 1 || t[1..].starts_with(' ') || t.chars().all(|x| x == c || x == ' '));
    let numbered = t.find(['.', ')']).is_some_and(|i| {
        i > 0 && i < 10 && t[..i].bytes().all(|b| b.is_ascii_digit()) && t[i + 1..].chars().next().is_none_or(|c| c == ' ')
    });
    let starts_block = t.starts_with(['#', '>', '%'])
        || marker('-')
        || marker('+')
        || marker('=')
        || t.starts_with(":::")
        || (t.starts_with('(') && t.contains(")="))
        || numbered;
    if starts_block { format!("\\{line}") } else { line.to_owned() }
}

fn link_dest(url: &str, title: Option<&str>) -> String {
    let url = if url.is_empty() || url.contains([' ', '(', ')', '<', '>']) { format!("<{}>", url.replace('>', "%3E")) } else { url.to_owned() };
    match title {
        Some(t) => format!("{url} \"{}\"", t.replace('"', "\\\"")),
        None => url,
    }
}

/// `key: value` frontmatter / directive option values. JSON is YAML, so
/// strings and structures are written as JSON.
fn yaml(value: &Value) -> String {
    match value {
        Value::String(t) if !t.is_empty() && !t.contains(['\n', ':', '#', '"', '\'']) && t.trim() == t => t.clone(),
        other => other.to_string(),
    }
}

impl Writer<'_> {
    /// A document: frontmatter (with `title`), then its blocks.
    pub fn document(&self, root: &Value, title: &str) -> String {
        let mut front = root.get("frontmatter").and_then(Value::as_object).cloned().unwrap_or_default();
        if !title.is_empty() {
            front.insert("title".to_owned(), Value::String(title.to_owned()));
        }
        let mut out = String::new();
        if !front.is_empty() {
            out.push_str("---\n");
            if let Some(t) = front.remove("title") {
                writeln!(out, "title: {}", yaml(&t)).unwrap();
            }
            for (k, v) in &front {
                writeln!(out, "{k}: {}", yaml(v)).unwrap();
            }
            out.push_str("---\n\n");
        }
        let body = self.blocks_of(root);
        out.push_str(body.trim_end());
        out.push('\n');
        out
    }

    /// Children of a root: `block` nodes are separated by `+++`.
    fn blocks_of(&self, root: &Value) -> String {
        let mut parts = Vec::new();
        for (i, child) in kids(root).iter().enumerate() {
            if ty(child) == "block" {
                let marker = match s(child, "meta") {
                    Some(meta) => format!("+++ {meta}"),
                    None => "+++".to_owned(),
                };
                if i > 0 || child.get("meta").is_some() {
                    parts.push(marker);
                }
                let body = self.flow(kids(child), false);
                if !body.is_empty() {
                    parts.push(body);
                }
            } else {
                parts.push(self.flow(std::slice::from_ref(child), false));
            }
        }
        parts.retain(|p| !p.is_empty());
        parts.join("\n\n")
    }

    /// Block content, one block after another; `tight` lists put no blank
    /// line between paragraphs.
    fn flow(&self, nodes: &[Value], tight: bool) -> String {
        let parts: Vec<String> = nodes.iter().map(|n| self.block(n)).filter(|b| !b.is_empty()).collect();
        parts.join(if tight { "\n" } else { "\n\n" })
    }

    fn block(&self, node: &Value) -> String {
        let body = self.block_body(node);
        match s(node, "id").and_then(|id| (self.targets)(id)) {
            Some(label) if !body.is_empty() => format!("({label})=\n{body}"),
            _ => body,
        }
    }

    fn block_body(&self, node: &Value) -> String {
        match ty(node) {
            "paragraph" => self.paragraph(kids(node)),
            "heading" => {
                let depth = node.get("depth").and_then(Value::as_u64).unwrap_or(1).clamp(1, 6) as usize;
                format!("{} {}", "#".repeat(depth), self.inline(kids(node)).replace('\n', " "))
            }
            "blockquote" => indent(&self.flow(kids(node), false), "> ", "> "),
            "list" => self.list(node),
            "thematicBreak" => "---".to_owned(),
            "code" => {
                let value = s(node, "value").unwrap_or("").trim_end_matches('\n');
                let info = [s(node, "lang"), s(node, "meta")].into_iter().flatten().collect::<Vec<_>>().join(" ");
                let f = fence(value);
                format!("{f}{info}\n{value}\n{f}")
            }
            "math" => {
                let value = s(node, "value").unwrap_or("").trim_end_matches('\n');
                match s(node, "label") {
                    Some(label) => {
                        let f = fence(value);
                        format!("{f}{{math}}\n:label: {label}\n{value}\n{f}")
                    }
                    None => format!("$$\n{value}\n$$"),
                }
            }
            "html" => s(node, "value").unwrap_or("").to_owned(),
            "image" => self.image(node),
            "mystTarget" => format!("({})=", s(node, "label").unwrap_or("")),
            "mystComment" => indent(s(node, "value").unwrap_or(""), "% ", "% "),
            "mystDirective" => self.directive(node),
            "admonition" => {
                let kind = s(node, "kind").unwrap_or("note");
                let (title, body): (Vec<&Value>, Vec<&Value>) = kids(node).iter().partition(|c| ty(c) == "admonitionTitle");
                let args = title.first().map(|t| self.inline(kids(t))).unwrap_or_default();
                let mut options = Map::new();
                if let Some(class) = node.get("class") {
                    options.insert("class".to_owned(), class.clone());
                }
                self.fenced(kind, &args, &options, &self.flow(&body.into_iter().cloned().collect::<Vec<_>>(), false))
            }
            "proof" => {
                let kind = s(node, "kind").unwrap_or("proof");
                let (title, body): (Vec<&Value>, Vec<&Value>) = kids(node).iter().partition(|c| ty(c) == "admonitionTitle");
                let args = title.first().map(|t| self.inline(kids(t))).unwrap_or_default();
                let mut options = Map::new();
                if let Some(label) = node.get("label") {
                    options.insert("label".to_owned(), label.clone());
                }
                self.fenced(&format!("prf:{kind}"), &args, &options, &self.flow(&body.into_iter().cloned().collect::<Vec<_>>(), false))
            }
            "container" => self.container(node),
            "table" => self.table(node),
            "footnoteDefinition" => {
                let body = self.flow(kids(node), false);
                indent(&body, &format!("[^{}]: ", s(node, "label").or(s(node, "identifier")).unwrap_or("")), "    ")
            }
            // Pieces of a container written by their parent; alone, their content.
            "caption" | "legend" | "admonitionTitle" | "block" | "root" | "listItem" => self.flow(kids(node), false),
            // Phrasing where a block is expected, or an unknown type: keep its content.
            _ if kids(node).iter().all(|c| is_phrasing(ty(c))) && !kids(node).is_empty() => self.paragraph(kids(node)),
            _ if !kids(node).is_empty() => self.flow(kids(node), false),
            _ => escape_line_start(&escape(&text(node))),
        }
    }

    fn paragraph(&self, children: &[Value]) -> String {
        self.inline(children).split('\n').map(escape_line_start).collect::<Vec<_>>().join("\n")
    }

    fn list(&self, node: &Value) -> String {
        let ordered = node.get("ordered").and_then(Value::as_bool).unwrap_or(false);
        let start = node.get("start").and_then(Value::as_u64).unwrap_or(1);
        let spread = node.get("spread").and_then(Value::as_bool).unwrap_or(false);
        let items: Vec<String> = kids(node)
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let marker = if ordered { format!("{}. ", start + i as u64) } else { "- ".to_owned() };
                let check = match item.get("checked").and_then(Value::as_bool) {
                    Some(true) => "[x] ",
                    Some(false) => "[ ] ",
                    None => "",
                };
                let item_spread = item.get("spread").and_then(Value::as_bool).unwrap_or(spread);
                let body = self.flow(kids(item), !item_spread);
                indent(&format!("{check}{body}"), &marker, &" ".repeat(marker.len()))
            })
            .collect();
        items.join(if spread { "\n\n" } else { "\n" })
    }

    fn image(&self, node: &Value) -> String {
        let url = s(node, "url").unwrap_or("");
        let extra: Vec<(&str, &Value)> =
            ["width", "height", "align", "class"].into_iter().filter_map(|k| node.get(k).map(|v| (k, v))).collect();
        if extra.is_empty() {
            return format!("![{}]({})", escape(s(node, "alt").unwrap_or("")), link_dest(url, s(node, "title")));
        }
        let mut options = Map::new();
        if let Some(alt) = node.get("alt") {
            options.insert("alt".to_owned(), alt.clone());
        }
        for (k, v) in extra {
            options.insert(k.to_owned(), v.clone());
        }
        self.fenced("image", url, &options, "")
    }

    fn container(&self, node: &Value) -> String {
        let image = kids(node).iter().find(|c| ty(c) == "image");
        let mut options = Map::new();
        if let Some(label) = node.get("label") {
            options.insert("label".to_owned(), label.clone());
        }
        if let Some(width) = image.and_then(|i| i.get("width")) {
            options.insert("width".to_owned(), width.clone());
        }
        let body: Vec<String> = kids(node)
            .iter()
            .filter(|c| ty(c) != "image")
            .map(|c| if matches!(ty(c), "caption" | "legend") { self.flow(kids(c), false) } else { self.block(c) })
            .collect();
        let url = image.and_then(|i| s(i, "url")).unwrap_or("");
        self.fenced("figure", url, &options, &body.join("\n\n"))
    }

    fn directive(&self, node: &Value) -> String {
        let name = s(node, "name").unwrap_or("");
        let args = s(node, "args").unwrap_or("");
        let options = node.get("options").and_then(Value::as_object).cloned().unwrap_or_default();
        let body = match s(node, "value") {
            Some(value) => value.to_owned(),
            // No source kept: write the parsed content back, minus what the
            // arguments already say (the figure's image, the title).
            None => kids(node)
                .first()
                .map(|parsed| {
                    let parts: Vec<String> = kids(parsed)
                        .iter()
                        .filter(|c| !matches!(ty(c), "image" | "admonitionTitle"))
                        .map(|c| if matches!(ty(c), "caption" | "legend") { self.flow(kids(c), false) } else { self.block(c) })
                        .collect();
                    parts.join("\n\n")
                })
                .unwrap_or_default(),
        };
        self.fenced(name, args, &options, &body)
    }

    /// A fenced directive: ```{name} args, `:key: value` options, the body.
    fn fenced(&self, name: &str, args: &str, options: &Map<String, Value>, body: &str) -> String {
        let f = fence(body);
        let mut out = format!("{f}{{{name}}}");
        if !args.is_empty() {
            write!(out, " {args}").unwrap();
        }
        for (k, v) in options {
            write!(out, "\n:{k}: {}", yaml(v)).unwrap();
        }
        if !body.is_empty() {
            write!(out, "\n{}", body.trim_end_matches('\n')).unwrap();
        }
        write!(out, "\n{f}").unwrap();
        out
    }

    fn table(&self, node: &Value) -> String {
        let rows: Vec<Vec<String>> = kids(node)
            .iter()
            .map(|row| kids(row).iter().map(|cell| self.inline(kids(cell)).replace('\n', " ").replace('|', "\\|")).collect())
            .collect();
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return String::new();
        }
        let align: Vec<Option<&str>> =
            (0..width).map(|i| node.get("align").and_then(|a| a.get(i)).and_then(Value::as_str)).collect();
        let line = |cells: &[String]| {
            let padded: Vec<&str> = (0..width).map(|i| cells.get(i).map_or("", String::as_str)).collect();
            format!("| {} |", padded.join(" | "))
        };
        let rule: Vec<&str> = align
            .iter()
            .map(|a| match a {
                Some("left") => ":--",
                Some("right") => "--:",
                Some("center") => ":-:",
                _ => "---",
            })
            .collect();
        let mut out = vec![line(&rows[0]), format!("| {} |", rule.join(" | "))];
        out.extend(rows[1..].iter().map(|r| line(r)));
        out.join("\n")
    }

    // ---- Inline -----------------------------------------------------------

    fn inline(&self, nodes: &[Value]) -> String {
        nodes.iter().map(|n| self.phrase(n)).collect()
    }

    fn role(name: &str, body: &str) -> String {
        let f = "`".repeat(body.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1);
        let pad = if body.starts_with('`') || body.ends_with('`') { " " } else { "" };
        format!("{{{name}}}{f}{pad}{body}{pad}{f}")
    }

    fn phrase(&self, node: &Value) -> String {
        match ty(node) {
            "text" => escape(s(node, "value").unwrap_or("")),
            "emphasis" => format!("*{}*", self.inline(kids(node))),
            "strong" => format!("**{}**", self.inline(kids(node))),
            "delete" => format!("~~{}~~", self.inline(kids(node))),
            "underline" => Writer::role("u", &text(node)),
            "subscript" => Writer::role("sub", &text(node)),
            "superscript" => Writer::role("sup", &text(node)),
            "inlineCode" => {
                let value = s(node, "value").unwrap_or("");
                let f = "`".repeat(value.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1);
                let pad = if value.starts_with('`') || value.ends_with('`') { " " } else { "" };
                format!("{f}{pad}{value}{pad}{f}")
            }
            "inlineMath" => {
                let value = s(node, "value").unwrap_or("");
                if value.contains('$') { Writer::role("math", value) } else { format!("${value}$") }
            }
            "break" => "\\\n".to_owned(),
            "image" => format!("![{}]({})", escape(s(node, "alt").unwrap_or("")), link_dest(s(node, "url").unwrap_or(""), s(node, "title"))),
            "link" => {
                let url = s(node, "url").unwrap_or("");
                let url = (self.links)(url).unwrap_or_else(|| url.to_owned());
                format!("[{}]({})", self.inline(kids(node)), link_dest(&url, s(node, "title")))
            }
            "crossReference" => {
                let label = s(node, "label").or(s(node, "identifier")).unwrap_or("");
                format!("[{}](#{label})", self.inline(kids(node)))
            }
            "citeGroup" => {
                let narrative = s(node, "kind") == Some("narrative");
                let cites: Vec<String> = kids(node).iter().map(|c| self.cite(c)).collect();
                if narrative { cites.join("; ") } else { format!("[{}]", cites.join("; ")) }
            }
            "cite" => self.cite(node),
            "footnoteReference" => format!("[^{}]", s(node, "label").or(s(node, "identifier")).unwrap_or("")),
            "mystRole" => Writer::role(s(node, "name").unwrap_or(""), s(node, "value").unwrap_or("")),
            "abbreviation" => match s(node, "title") {
                Some(title) => Writer::role("abbr", &format!("{} ({title})", text(node))),
                None => escape(&text(node)),
            },
            _ if !kids(node).is_empty() => self.inline(kids(node)),
            _ => escape(&text(node)),
        }
    }

    fn cite(&self, node: &Value) -> String {
        let mut out = String::new();
        if let Some(prefix) = s(node, "prefix") {
            write!(out, "{prefix} ").unwrap();
        }
        write!(out, "@{}", s(node, "label").or(s(node, "identifier")).unwrap_or("")).unwrap();
        if let Some(suffix) = s(node, "suffix") {
            write!(out, ", {suffix}").unwrap();
        }
        out
    }
}

fn is_phrasing(t: &str) -> bool {
    crate::myst::TEXT_RUN_TYPES.contains(&t)
        || matches!(t, "link" | "crossReference" | "cite" | "citeGroup" | "footnoteReference" | "mystRole" | "image")
}
