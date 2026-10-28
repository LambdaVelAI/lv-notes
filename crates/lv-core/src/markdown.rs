//! Reading Markdown: MyST Markdown (Jupyter Book 2) and Obsidian notes, into
//! a MyST AST (as JSON) ready for `convert::import_document`.
//!
//! markdown-rs parses CommonMark with GFM, math and frontmatter into mdast,
//! which is already MyST's shape. The MyST and Obsidian syntax CommonMark
//! doesn't know arrives as plain text or code, and the transforms here turn
//! it into MyST nodes:
//!
//! - ```` ```{name} args ```` and `:::{name}` fences → directives; the common
//!   ones (admonitions, `prf:` kinds, `math`, `image`, `figure`) become their
//!   MyST nodes, the rest stay `mystDirective` with their source;
//! - `{name}` before inline code → roles (`sub`, `sup`, `u`, `abbr`, `math`,
//!   `ref`, `cite`, ...);
//! - `(label)=` lines → `mystTarget`, `% ...` → `mystComment`, `+++` → block
//!   breaks;
//! - `@key` and `[see @key, p. 2; @other]` → citations;
//! - `[text](#label)` → cross-references;
//! - `> [!note] Title` → admonitions (Obsidian callouts, GitHub alerts).
//!
//! Obsidian adds `[[wikilinks]]`, `^block-ids` and `%% comments %%`. A
//! wikilink becomes a `link` with a `_wikilink` field, and a block ID a
//! `_block_id` field, for the folder importer (`import.rs`) to resolve; it
//! removes them. Anything unrecognized stays as text: nothing is dropped.

use serde_json::{Map, Value, json};

use crate::myst::TEXT_RUN_TYPES;

/// Which Markdown dialect a file is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// MyST Markdown, as Jupyter Book 2 (mystmd) reads it.
    Myst,
    /// Obsidian: CommonMark plus wikilinks, block IDs and `%%` comments.
    Obsidian,
}

/// Admonition kinds MyST knows as directives.
const ADMONITIONS: [&str; 11] =
    ["note", "tip", "hint", "important", "warning", "caution", "attention", "danger", "error", "seealso", "admonition"];

/// Parses `text` into a MyST `root` (as JSON), without node IDs.
pub fn parse(text: &str, dialect: Dialect) -> Value {
    let source = colon_fences(text);
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.math_flow = true;
    options.constructs.math_text = true;
    options.constructs.frontmatter = true;
    // Without MDX enabled, parsing cannot fail.
    let tree = markdown::to_mdast(&source, &options).expect("markdown parses");
    let mut root = serde_json::to_value(&tree).expect("mdast serializes");
    strip_positions(&mut root);
    let mut reader = Reader { dialect, definitions: Map::new() };
    reader.collect_definitions(&root);
    reader.root(root)
}

/// YAML frontmatter as JSON; `None` if it doesn't parse or isn't a mapping.
pub fn yaml_to_json(text: &str) -> Option<Value> {
    let docs = yaml_rust2::YamlLoader::load_from_str(text).ok()?;
    let value = yaml(docs.into_iter().next()?);
    value.is_object().then_some(value)
}

fn yaml(y: yaml_rust2::Yaml) -> Value {
    use yaml_rust2::Yaml;
    match y {
        Yaml::Real(s) => s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::String(s), Value::Number),
        Yaml::Integer(i) => json!(i),
        Yaml::String(s) => Value::String(s),
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Array(items) => Value::Array(items.into_iter().map(yaml).collect()),
        Yaml::Hash(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let key = match yaml(k) {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (key, yaml(v))
                })
                .collect(),
        ),
        Yaml::Alias(_) | Yaml::Null | Yaml::BadValue => Value::Null,
    }
}

/// Rewrites `:::{name}` colon fences as backtick fences, which CommonMark
/// parses as code and the reader then turns into directives. Longer fences
/// stay longer (an outer `::::` encloses an inner `:::`), and every fence is
/// longer than an ordinary ```` ``` ```` code block inside it.
fn colon_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open: Vec<usize> = Vec::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let colons = trimmed.chars().take_while(|&c| c == ':').count();
        let rest = trimmed[colons..].trim_end();
        if colons >= 3 && rest.starts_with('{') {
            open.push(colons);
            out.push_str(&format!("{}{}\n", "`".repeat(colons + 3), rest));
        } else if colons >= 3 && rest.is_empty() && open.last() == Some(&colons) {
            open.pop();
            out.push_str(&format!("{}\n", "`".repeat(colons + 3)));
        } else {
            out.push_str(line);
        }
    }
    out
}

fn strip_positions(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("position");
            map.values_mut().for_each(strip_positions);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_positions),
        _ => {}
    }
}

fn ty(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

fn s<'v>(node: &'v Value, key: &str) -> Option<&'v str> {
    node.get(key).and_then(Value::as_str)
}

fn text_node(value: impl Into<String>) -> Value {
    json!({ "type": "text", "value": value.into() })
}

fn take_children(node: &mut Value) -> Vec<Value> {
    match node.get_mut("children").map(Value::take) {
        Some(Value::Array(items)) => items,
        _ => vec![],
    }
}

/// The plain text of a node.
fn text_of(node: &Value) -> String {
    match s(node, "value") {
        Some(v) => v.to_owned(),
        None => node.get("children").and_then(Value::as_array).map_or(String::new(), |c| c.iter().map(text_of).collect()),
    }
}

/// A label's MyST identifier: lowercased, whitespace collapsed.
pub fn identifier(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

struct Reader {
    dialect: Dialect,
    /// Link reference definitions: identifier → (url, title).
    definitions: Map<String, Value>,
}

impl Reader {
    fn collect_definitions(&mut self, node: &Value) {
        if ty(node) == "definition"
            && let Some(id) = s(node, "identifier")
        {
            self.definitions.insert(id.to_owned(), json!({ "url": node["url"], "title": node["title"] }));
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            children.iter().for_each(|c| self.collect_definitions(c));
        }
    }

    fn root(&mut self, mut root: Value) -> Value {
        let children = take_children(&mut root);
        let mut frontmatter = None;
        let mut rest = Vec::new();
        for child in children {
            if ty(&child) == "yaml" {
                frontmatter = s(&child, "value").and_then(yaml_to_json);
            } else {
                rest.push(child);
            }
        }
        let flow = self.flow(rest, true);

        // `+++` markers split the root into blocks.
        let mut out = json!({ "type": "root", "children": [] });
        if let Some(fm) = frontmatter {
            out["frontmatter"] = fm;
        }
        if !flow.iter().any(|n| ty(n) == "_break") {
            out["children"] = Value::Array(flow);
            return out;
        }
        let mut blocks = Vec::new();
        let mut current = json!({ "type": "block", "children": [] });
        let mut started = false;
        for node in flow {
            if ty(&node) == "_break" {
                if started || !current["children"].as_array().unwrap().is_empty() {
                    blocks.push(current);
                }
                current = json!({ "type": "block", "children": [] });
                if let Some(meta) = s(&node, "meta").filter(|m| !m.is_empty()) {
                    current["meta"] = Value::String(meta.to_owned());
                }
                started = true;
            } else {
                current["children"].as_array_mut().unwrap().push(node);
            }
        }
        blocks.push(current);
        out["children"] = Value::Array(blocks);
        out
    }

    /// Block content: each node transformed, with special paragraphs split
    /// into targets, comments and block breaks.
    fn flow(&mut self, nodes: Vec<Value>, top: bool) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for node in nodes {
            match ty(&node) {
                "definition" => {}
                "paragraph" => self.paragraph(node, top, &mut out),
                _ => out.push(self.block(node)),
            }
        }
        out
    }

    fn paragraph(&mut self, mut node: Value, top: bool, out: &mut Vec<Value>) {
        // A target line directly above a paragraph with formatting in it.
        if let Some(first) = node["children"].get(0).filter(|c| ty(c) == "text").and_then(|c| s(c, "value"))
            && let Some((line, rest)) = first.split_once('\n')
            && let Some(label) = self.target_line(line.trim())
        {
            out.push(json!({ "type": "mystTarget", "label": label }));
            let rest = rest.to_owned();
            node["children"][0]["value"] = Value::String(rest);
        }
        let only_text = node["children"].as_array().is_some_and(|c| c.iter().all(|c| ty(c) == "text"));
        let raw = text_of(&node);

        if only_text {
            // Obsidian: a `^id` on its own line names the block before it.
            if self.dialect == Dialect::Obsidian
                && let Some(id) = whole_block_id(raw.trim())
                && let Some(previous) = out.last_mut()
            {
                previous["_block_id"] = Value::String(id.to_owned());
                return;
            }
            if let Some(special) = self.special_lines(&raw, top) {
                for piece in special {
                    match piece {
                        Special::Node(n) => out.push(n),
                        Special::Rest(text) => {
                            let p = json!({ "type": "paragraph", "children": [text_node(text)] });
                            self.paragraph(p, top, out);
                        }
                    }
                }
                return;
            }
        }

        let children = take_children(&mut node);
        let mut children = self.inline(children);
        if self.dialect == Dialect::Obsidian
            && let Some(id) = trailing_block_id(&mut children)
        {
            node["_block_id"] = Value::String(id);
        }
        // A paragraph that is just an image is an image block.
        if let [only] = children.as_slice()
            && ty(only) == "image"
        {
            let mut image = only.clone();
            if let Some(id) = node.get("_block_id") {
                image["_block_id"] = id.clone();
            }
            out.push(image);
            return;
        }
        node["children"] = Value::Array(children);
        out.push(node);
    }

    /// The label of a target line: `(label)=`, or in Obsidian `%% (label)= %%`.
    fn target_line(&self, line: &str) -> Option<String> {
        let line = match self.dialect {
            Dialect::Myst => line,
            Dialect::Obsidian => line.strip_prefix("%%")?.strip_suffix("%%")?.trim(),
        };
        line.strip_prefix('(').and_then(|l| l.strip_suffix(")=")).map(str::to_owned)
    }

    /// Paragraph lines that are MyST or Obsidian syntax of their own:
    /// targets, comments, block breaks. `None` if the first line is none.
    fn special_lines(&self, raw: &str, top: bool) -> Option<Vec<Special>> {
        let (first, rest) = raw.split_once('\n').unwrap_or((raw, ""));
        let first = first.trim();
        let node = match self.dialect {
            Dialect::Myst => {
                if let Some(label) = first.strip_prefix('(').and_then(|l| l.strip_suffix(")=")) {
                    json!({ "type": "mystTarget", "label": label })
                } else if first.starts_with('%') {
                    // A comment runs while its lines start with `%`.
                    let lines: Vec<&str> = raw.lines().collect();
                    let n = lines.iter().take_while(|l| l.trim_start().starts_with('%')).count();
                    let value = lines[..n].iter().map(|l| l.trim_start()[1..].strip_prefix(' ').unwrap_or(&l.trim_start()[1..])).collect::<Vec<_>>().join("\n");
                    let mut pieces = vec![Special::Node(json!({ "type": "mystComment", "value": value }))];
                    let after = lines[n..].join("\n");
                    if !after.trim().is_empty() {
                        pieces.push(Special::Rest(after));
                    }
                    return Some(pieces);
                } else if top && (first == "+++" || first.starts_with("+++ ")) {
                    json!({ "type": "_break", "meta": first[3..].trim() })
                } else {
                    return None;
                }
            }
            Dialect::Obsidian => {
                let inner = first.strip_prefix("%%")?.strip_suffix("%%")?.trim();
                if let Some(label) = inner.strip_prefix('(').and_then(|l| l.strip_suffix(")=")) {
                    json!({ "type": "mystTarget", "label": label })
                } else if top && (inner == "+++" || inner.starts_with("+++ ")) {
                    json!({ "type": "_break", "meta": inner[3..].trim() })
                } else {
                    json!({ "type": "mystComment", "value": inner })
                }
            }
        };
        let mut pieces = vec![Special::Node(node)];
        if !rest.trim().is_empty() {
            pieces.push(Special::Rest(rest.to_owned()));
        }
        Some(pieces)
    }

    fn block(&mut self, mut node: Value) -> Value {
        match ty(&node) {
            "code" => {
                let lang = s(&node, "lang").unwrap_or("");
                if let Some(name) = lang.strip_prefix('{').and_then(|l| l.strip_suffix('}')) {
                    let args = s(&node, "meta").unwrap_or("").to_owned();
                    let body = s(&node, "value").unwrap_or("").to_owned();
                    return self.directive(name, &args, &body);
                }
                node
            }
            "blockquote" => {
                let children = take_children(&mut node);
                if let Some(callout) = self.callout(&children) {
                    return callout;
                }
                node["children"] = Value::Array(self.flow(children, false));
                node
            }
            "heading" | "tableCell" => {
                let children = take_children(&mut node);
                let mut children = self.inline(children);
                if self.dialect == Dialect::Obsidian
                    && let Some(id) = trailing_block_id(&mut children)
                {
                    node["_block_id"] = Value::String(id);
                }
                node["children"] = Value::Array(children);
                // Like mystmd, a MyST heading is labelled by its text, so
                // `[x](file.md#on-reading-slowly)` finds it.
                if self.dialect == Dialect::Myst && ty(&node) == "heading" && node.get("identifier").is_none() {
                    let label = text_of(&node).trim().to_owned();
                    if !label.is_empty() {
                        node["identifier"] = Value::String(crate::export::slug(&label));
                        node["label"] = Value::String(label);
                    }
                }
                node
            }
            "list" | "listItem" | "footnoteDefinition" | "table" | "tableRow" => {
                let children = take_children(&mut node);
                let children = if ty(&node) == "tableRow" || ty(&node) == "table" {
                    children.into_iter().map(|c| self.block(c)).collect()
                } else {
                    self.flow(children, false)
                };
                node["children"] = Value::Array(children);
                node
            }
            _ => node,
        }
    }

    /// `> [!kind] Title` (Obsidian callouts, GitHub alerts).
    fn callout(&mut self, children: &[Value]) -> Option<Value> {
        let first = children.first().filter(|p| ty(p) == "paragraph")?;
        let first_text = first["children"].get(0).filter(|t| ty(t) == "text").and_then(|t| s(t, "value"))?;
        let rest = first_text.strip_prefix("[!")?;
        let close = rest.find(']')?;
        let kind = rest[..close].to_lowercase();
        if kind.is_empty() || !kind.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':') {
            return None;
        }
        // `[!note]-` / `[!note]+` fold markers are dropped.
        let after = rest[close + 1..].trim_start_matches(['-', '+']);
        let (title_line, body_line) = after.split_once('\n').unwrap_or((after, ""));

        let mut first = first.clone();
        let mut inline = first["children"].as_array().cloned().unwrap_or_default();
        // The title is the rest of the first line (plus any inline nodes on it).
        let mut title: Vec<Value> = Vec::new();
        let mut body: Vec<Value> = Vec::new();
        if !title_line.trim().is_empty() {
            title.push(text_node(title_line.trim().to_owned()));
        }
        if !body_line.is_empty() || inline.len() > 1 {
            if body_line.is_empty() {
                // Inline nodes after the title stay on the title line.
                title.extend(inline.drain(1..));
            } else {
                inline[0] = text_node(body_line.to_owned());
                first["children"] = Value::Array(inline);
                body.push(first);
            }
        }
        body.extend(children[1..].iter().cloned());
        let mut body = self.flow(body, false);
        let title = self.inline(title);

        let (node_type, kind) = match kind.strip_prefix("prf:") {
            Some(k) => ("proof", k.to_owned()),
            None if is_proof_kind(&kind) => ("proof", kind),
            None => ("admonition", kind),
        };
        let mut out = json!({ "type": node_type, "kind": kind, "children": [] });
        if !title.is_empty() {
            body.insert(0, json!({ "type": "admonitionTitle", "children": title }));
        }
        out["children"] = Value::Array(body);
        Some(out)
    }

    /// A MyST directive from its fence: known ones become their MyST nodes,
    /// the rest a `mystDirective` holding its source.
    fn directive(&mut self, name: &str, args: &str, body: &str) -> Value {
        let (options, content) = directive_options(body);
        let parse_body = |reader: &mut Reader| -> Vec<Value> {
            let inner = parse(content, reader.dialect);
            inner.get("children").and_then(Value::as_array).cloned().unwrap_or_default()
        };
        let label = options.get("label").and_then(Value::as_str).map(str::to_owned);
        let with_label = |mut node: Value| {
            if let Some(label) = &label {
                node["label"] = Value::String(label.clone());
                node["identifier"] = Value::String(identifier(label));
            }
            node
        };
        let title = |reader: &mut Reader| -> Option<Value> {
            if args.trim().is_empty() {
                return None;
            }
            let inner = parse(args.trim(), reader.dialect);
            let inline = inner["children"].get(0).filter(|p| ty(p) == "paragraph").map_or_else(|| vec![text_node(args.trim())], |p| {
                p["children"].as_array().cloned().unwrap_or_default()
            });
            Some(json!({ "type": "admonitionTitle", "children": inline }))
        };

        let kind_name = name.strip_prefix("prf:");
        if ADMONITIONS.contains(&name) || kind_name.is_some() {
            let mut children = Vec::new();
            if let Some(t) = title(self) {
                children.push(t);
            }
            children.extend(parse_body(self));
            let mut node = match kind_name {
                Some(kind) => json!({ "type": "proof", "kind": kind }),
                None => json!({ "type": "admonition", "kind": name }),
            };
            if let Some(class) = options.get("class") {
                node["class"] = class.clone();
            }
            node["children"] = Value::Array(children);
            return with_label(node);
        }
        match name {
            "math" => with_label(json!({ "type": "math", "value": content.trim_end_matches('\n') })),
            "image" => {
                let mut node = json!({ "type": "image", "url": args.trim() });
                for (k, v) in &options {
                    node[k] = v.clone();
                }
                node
            }
            "figure" => {
                let mut image = json!({ "type": "image", "url": args.trim() });
                for key in ["width", "height", "alt"] {
                    if let Some(v) = options.get(key) {
                        image[key] = v.clone();
                    }
                }
                let mut children = vec![image];
                let mut paragraphs = parse_body(self).into_iter();
                if let Some(caption) = paragraphs.next() {
                    children.push(json!({ "type": "caption", "children": [caption] }));
                }
                let legend: Vec<Value> = paragraphs.collect();
                if !legend.is_empty() {
                    children.push(json!({ "type": "legend", "children": legend }));
                }
                with_label(json!({ "type": "container", "kind": "figure", "children": children }))
            }
            _ => {
                let mut node = json!({ "type": "mystDirective", "name": name });
                if !args.is_empty() {
                    node["args"] = Value::String(args.to_owned());
                }
                if !options.is_empty() {
                    node["options"] = Value::Object(options);
                }
                if !content.is_empty() {
                    node["value"] = Value::String(content.trim_end_matches('\n').to_owned());
                }
                node
            }
        }
    }

    // ---- Inline -------------------------------------------------------------

    fn inline(&mut self, nodes: Vec<Value>) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut iter = nodes.into_iter().peekable();
        while let Some(mut node) = iter.next() {
            match ty(&node) {
                "text" => {
                    let value = s(&node, "value").unwrap_or("").to_owned();
                    // `{name}` right before inline code is a role.
                    if let Some(next) = iter.peek()
                        && ty(next) == "inlineCode"
                        && let Some((before, name)) = role_name(&value)
                    {
                        let body = s(next, "value").unwrap_or("").to_owned();
                        iter.next();
                        if !before.is_empty() {
                            out.extend(self.text(before));
                        }
                        out.push(role(name, &body));
                        continue;
                    }
                    out.extend(self.text(&value));
                }
                "link" => {
                    let url = s(&node, "url").unwrap_or("").to_owned();
                    let children = take_children(&mut node);
                    let children = self.inline(children);
                    if let Some(label) = url.strip_prefix('#').filter(|l| !l.is_empty() && !l.starts_with("node-")) {
                        out.push(json!({ "type": "crossReference", "identifier": identifier(label), "label": label, "children": children }));
                    } else {
                        node["children"] = Value::Array(children);
                        if node.get("title").is_some_and(Value::is_null) {
                            node.as_object_mut().unwrap().remove("title");
                        }
                        out.push(node);
                    }
                }
                "linkReference" | "imageReference" => {
                    let id = s(&node, "identifier").unwrap_or("").to_owned();
                    let Some(def) = self.definitions.get(&id).cloned() else {
                        // No definition: it was only ever text.
                        out.push(text_node(format!("[{}]", text_of(&node))));
                        continue;
                    };
                    let is_image = ty(&node) == "imageReference";
                    let mut link = json!({ "type": if is_image { "image" } else { "link" }, "url": def["url"] });
                    if let Some(t) = def["title"].as_str() {
                        link["title"] = Value::String(t.to_owned());
                    }
                    if is_image {
                        link["alt"] = node["alt"].clone();
                    } else {
                        let children = take_children(&mut node);
                        link["children"] = Value::Array(self.inline(children));
                    }
                    out.push(link);
                }
                "image" => {
                    if node.get("title").is_some_and(Value::is_null) {
                        node.as_object_mut().unwrap().remove("title");
                    }
                    out.push(node);
                }
                t if TEXT_RUN_TYPES.contains(&t) && node.get("children").is_some() => {
                    let children = take_children(&mut node);
                    node["children"] = Value::Array(self.inline(children));
                    out.push(node);
                }
                _ => out.push(node),
            }
        }
        merge_text(html_marks(out))
    }

    /// A text run: citations, and in Obsidian wikilinks.
    fn text(&self, value: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let mut rest = value;
        while !rest.is_empty() {
            let next = [
                (self.dialect == Dialect::Obsidian).then(|| find_wikilink(rest)).flatten().map(|i| (i, Piece::Wiki)),
                find_cite_group(rest).map(|(i, _)| (i, Piece::Group)),
                find_narrative(rest).map(|(i, _)| (i, Piece::Narrative)),
            ]
            .into_iter()
            .flatten()
            .min_by_key(|(i, _)| *i);
            let Some((at, piece)) = next else {
                out.push(text_node(rest));
                break;
            };
            let parsed = match piece {
                Piece::Wiki => wikilink(&rest[at..]),
                Piece::Group => find_cite_group(&rest[at..]).filter(|(i, _)| *i == 0).map(|(_, (len, node))| (len, node)),
                Piece::Narrative => find_narrative(&rest[at..]).filter(|(i, _)| *i == 0).map(|(_, (len, node))| (len, node)),
            };
            match parsed {
                Some((len, node)) => {
                    if at > 0 {
                        out.push(text_node(&rest[..at]));
                    }
                    out.push(node);
                    rest = &rest[at + len..];
                }
                None => {
                    // Not one after all: keep the opening character as text.
                    let step = at + rest[at..].chars().next().map_or(1, char::len_utf8);
                    out.push(text_node(&rest[..step]));
                    rest = &rest[step..];
                }
            }
        }
        merge_text(out)
    }
}

/// `<sub>`, `<sup>` and `<u>` around inline content → subscript,
/// superscript and underline.
fn html_marks(nodes: Vec<Value>) -> Vec<Value> {
    let tag = |n: &Value| (ty(n) == "html").then(|| s(n, "value")).flatten().map(str::to_owned);
    let mut out: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < nodes.len() {
        let kind = tag(&nodes[i]).and_then(|t| match t.as_str() {
            "<sub>" => Some(("subscript", "</sub>")),
            "<sup>" => Some(("superscript", "</sup>")),
            "<u>" => Some(("underline", "</u>")),
            _ => None,
        });
        if let Some((node_type, close)) = kind
            && let Some(end) = (i + 1..nodes.len()).find(|&j| tag(&nodes[j]).as_deref() == Some(close))
        {
            out.push(json!({ "type": node_type, "children": nodes[i + 1..end].to_vec() }));
            i = end + 1;
            continue;
        }
        out.push(nodes[i].clone());
        i += 1;
    }
    out
}

enum Special {
    Node(Value),
    Rest(String),
}

enum Piece {
    Wiki,
    Group,
    Narrative,
}

fn merge_text(nodes: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for node in nodes {
        if ty(&node) == "text"
            && let Some(last) = out.last_mut()
            && ty(last) == "text"
            && last.as_object().is_some_and(|m| m.len() == 2)
            && node.as_object().is_some_and(|m| m.len() == 2)
        {
            let joined = format!("{}{}", s(last, "value").unwrap_or(""), s(&node, "value").unwrap_or(""));
            last["value"] = Value::String(joined);
            continue;
        }
        if ty(&node) == "text" && s(&node, "value") == Some("") {
            continue;
        }
        out.push(node);
    }
    out
}

/// `text{name}` → (`text`, `name`) when the text ends in a role name.
fn role_name(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_suffix('}')?;
    let open = body.rfind('{')?;
    let name = &body[open + 1..];
    let ok = !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'));
    ok.then(|| (&text[..open], name))
}

fn role(name: &str, body: &str) -> Value {
    match name {
        "sub" | "subscript" => json!({ "type": "subscript", "children": [text_node(body)] }),
        "sup" | "superscript" => json!({ "type": "superscript", "children": [text_node(body)] }),
        "u" | "underline" => json!({ "type": "underline", "children": [text_node(body)] }),
        "del" | "strike" => json!({ "type": "delete", "children": [text_node(body)] }),
        "math" => json!({ "type": "inlineMath", "value": body }),
        "abbr" => match body.strip_suffix(')').and_then(|b| b.rsplit_once(" (")) {
            Some((short, title)) => json!({ "type": "abbreviation", "title": title, "children": [text_node(short)] }),
            None => json!({ "type": "abbreviation", "children": [text_node(body)] }),
        },
        "ref" | "numref" | "eq" | "doc" => {
            // `{ref}`text <label>`` or `{ref}`label``.
            let (text, label) = match body.strip_suffix('>').and_then(|b| b.rsplit_once(" <")) {
                Some((text, label)) => (Some(text), label),
                None => (None, body),
            };
            let mut node = json!({ "type": "crossReference", "identifier": identifier(label), "label": label });
            if name == "eq" {
                node["kind"] = json!("equation");
            }
            if let Some(text) = text {
                node["children"] = json!([text_node(text)]);
            }
            node
        }
        "cite" | "cite:p" | "cite:t" | "cite:ps" | "cite:ts" => {
            let narrative = name.starts_with("cite:t");
            let cites: Vec<Value> = body
                .split([',', ';'])
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(|k| cite(k, narrative, None, None))
                .collect();
            json!({ "type": "citeGroup", "kind": if narrative { "narrative" } else { "parenthetical" }, "children": cites })
        }
        _ => json!({ "type": "mystRole", "name": name, "value": body }),
    }
}

fn cite(key: &str, narrative: bool, prefix: Option<&str>, suffix: Option<&str>) -> Value {
    let mut node = json!({ "type": "cite", "label": key, "identifier": key.to_lowercase() });
    if narrative {
        node["kind"] = json!("narrative");
    }
    if let Some(p) = prefix.filter(|p| !p.is_empty()) {
        node["prefix"] = json!(p);
    }
    if let Some(sfx) = suffix.filter(|s| !s.is_empty()) {
        node["suffix"] = json!(sfx);
    }
    node
}

/// A citation key after `@`: letters, digits and `_:.-`, not ending in
/// punctuation. Returns its length in bytes.
fn citation_key(s: &str) -> Option<usize> {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if i == 0 && !(c.is_alphanumeric() || c == '_') {
            return None;
        }
        if c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '-') {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    let key = s[..end].trim_end_matches(['.', ':', '-']);
    (!key.is_empty()).then_some(key.len())
}

/// The first `@key` not inside a word (so not an email address).
fn find_narrative(text: &str) -> Option<(usize, (usize, Value))> {
    let mut from = 0;
    while let Some(i) = text[from..].find('@').map(|i| i + from) {
        let before = text[..i].chars().next_back();
        if before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.')) && let Some(len) = citation_key(&text[i + 1..]) {
            let key = &text[i + 1..i + 1 + len];
            let node = json!({ "type": "citeGroup", "kind": "narrative", "children": [cite(key, true, None, None)] });
            return Some((i, (len + 1, node)));
        }
        from = i + 1;
    }
    None
}

/// The first `[...]` whose parts, split by `;`, each hold one `@key`:
/// `[see @knuth1984, p. 97; @ramsey1994]`.
fn find_cite_group(text: &str) -> Option<(usize, (usize, Value))> {
    let mut from = 0;
    while let Some(open) = text[from..].find('[').map(|i| i + from) {
        if let Some(close) = text[open..].find(']').map(|i| i + open) {
            let inside = &text[open + 1..close];
            let cites: Option<Vec<Value>> = inside
                .split(';')
                .map(|part| {
                    let at = part.find('@')?;
                    let len = citation_key(&part[at + 1..])?;
                    let key = &part[at + 1..at + 1 + len];
                    let prefix = part[..at].trim();
                    let suffix = part[at + 1 + len..].trim().trim_start_matches(',').trim();
                    Some(cite(key, false, Some(prefix), Some(suffix)))
                })
                .collect();
            if let Some(cites) = cites.filter(|c| !c.is_empty()) {
                let node = json!({ "type": "citeGroup", "kind": "parenthetical", "children": cites });
                return Some((open, (close + 1 - open, node)));
            }
        }
        from = open + 1;
    }
    None
}

/// Where the first `[[` that isn't an embed (`![[`) starts.
fn find_wikilink(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find("[[").map(|i| i + from) {
        if !text[..i].ends_with('!') {
            return Some(i);
        }
        from = i + 2;
    }
    None
}

/// `[[target#fragment|alias]]` at the start of `text`, as a link the folder
/// importer resolves (`_wikilink`). Embeds (`![[...]]`) stay text.
fn wikilink(text: &str) -> Option<(usize, Value)> {
    let body = text.strip_prefix("[[")?;
    let end = body.find("]]")?;
    let inner = &body[..end];
    if inner.is_empty() || inner.contains('\n') || inner.contains("[[") {
        return None;
    }
    let (target, alias) = match inner.split_once('|') {
        Some((t, a)) => (t.trim(), Some(a.trim())),
        None => (inner.trim(), None),
    };
    let shown = alias.unwrap_or_else(|| target.trim_start_matches('#'));
    let node = json!({ "type": "link", "url": "", "_wikilink": target, "_alias": alias.is_some(), "children": [text_node(shown)] });
    Some((end + 4, node))
}

/// An Obsidian block ID that is the whole text: `^my-id`.
fn whole_block_id(text: &str) -> Option<&str> {
    let id = text.strip_prefix('^')?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(id)
}

/// Removes a trailing ` ^id` from inline content and returns the ID.
fn trailing_block_id(children: &mut Vec<Value>) -> Option<String> {
    let last = children.last_mut().filter(|n| ty(n) == "text")?;
    let value = s(last, "value")?.to_owned();
    let trimmed = value.trim_end();
    let caret = trimmed.rfind('^')?;
    let id = whole_block_id(&trimmed[caret..])?.to_owned();
    let before = &trimmed[..caret];
    if !(before.is_empty() || before.ends_with(char::is_whitespace)) {
        return None;
    }
    let before = before.trim_end().to_owned();
    if before.is_empty() {
        children.pop();
    } else {
        last["value"] = Value::String(before);
    }
    Some(id)
}

/// Directive options (`:key: value` lines at the start of the body) and the
/// rest of the body. Values written as JSON (strings in quotes, lists,
/// mappings) are read as JSON; anything else is a string.
fn directive_options(body: &str) -> (Map<String, Value>, &str) {
    let mut options = Map::new();
    let mut rest = body;
    while let Some(line) = rest.split_inclusive('\n').next() {
        let Some(option) = line.trim_end().strip_prefix(':') else { break };
        let Some((key, value)) = option.split_once(':') else { break };
        if key.is_empty() || key.contains(' ') {
            break;
        }
        let value = value.trim();
        let parsed = if value.starts_with(['"', '[', '{']) { serde_json::from_str(value).ok() } else { None };
        options.insert(key.to_owned(), parsed.unwrap_or_else(|| Value::String(value.to_owned())));
        rest = &rest[line.len()..];
    }
    (options, rest)
}

/// Callout types that are `prf:` kinds become proofs.
pub fn is_proof_kind(kind: &str) -> bool {
    matches!(
        kind,
        "theorem" | "lemma" | "corollary" | "proposition" | "conjecture" | "definition" | "axiom" | "assumption" | "remark"
            | "observation" | "property" | "criterion" | "notation" | "algorithm" | "proof"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str, dialect: Dialect) -> Value {
        parse(text, dialect)["children"].clone()
    }

    #[test]
    fn roles_cites_and_crossrefs() {
        let b = body("H{sub}`2`O, {abbr}`MyST (Markedly Structured Text)`, as @knuth says [see @a, p. 2; @b]. Mail a@b.org, [Fig](#fig-1).", Dialect::Myst);
        let p = &b[0]["children"];
        assert_eq!(p[1], json!({ "type": "subscript", "children": [{ "type": "text", "value": "2" }] }));
        assert_eq!(p[3]["title"], "Markedly Structured Text");
        assert_eq!(p[5]["children"][0]["label"], "knuth");
        assert_eq!(p[7]["kind"], "parenthetical");
        assert_eq!(p[7]["children"][0], json!({ "type": "cite", "label": "a", "identifier": "a", "prefix": "see", "suffix": "p. 2" }));
        // GFM makes the address a mail link; it is not a citation.
        assert_eq!(p[9]["url"], "mailto:a@b.org");
        assert_eq!(p[11]["type"], "crossReference");
    }

    #[test]
    fn directives_targets_comments_and_blocks() {
        let text = "Intro\n\n+++ {\"tags\": [\"x\"]}\n\n(eq-1)=\n```{math}\n:label: eq-1\nE = mc^2\n```\n\n:::{note} Careful\n:class: dropdown\nBody **here**.\n:::\n\n```{prf:theorem} Fermat\nNo.\n```\n\n% hidden\n\n```{custom} a b\n:opt: 1\nraw\n```";
        let root = parse(text, Dialect::Myst);
        let blocks = root["children"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["meta"], "{\"tags\": [\"x\"]}");
        let b = blocks[1]["children"].as_array().unwrap();
        assert_eq!(b[0], json!({ "type": "mystTarget", "label": "eq-1" }));
        assert_eq!(b[1], json!({ "type": "math", "value": "E = mc^2", "label": "eq-1", "identifier": "eq-1" }));
        assert_eq!((b[2]["type"].as_str(), b[2]["kind"].as_str(), b[2]["class"].as_str()), (Some("admonition"), Some("note"), Some("dropdown")));
        assert_eq!(b[2]["children"][0]["type"], "admonitionTitle");
        assert_eq!(b[2]["children"][1]["children"][1]["type"], "strong");
        assert_eq!((b[3]["type"].as_str(), b[3]["kind"].as_str()), (Some("proof"), Some("theorem")));
        assert_eq!(b[4], json!({ "type": "mystComment", "value": "hidden" }));
        assert_eq!(b[5], json!({ "type": "mystDirective", "name": "custom", "args": "a b", "options": { "opt": "1" }, "value": "raw" }));
    }

    #[test]
    fn obsidian_wikilinks_block_ids_callouts_and_comments() {
        let text = "See [[Other Note#^abc|that]] and [[Plain]] but ![[embed.png]].\nMore ^para-1\n\n> [!theorem] Fermat\n> Nothing.\n\n```\ncode\n```\n^code-id\n\n%% +++ %%\n\n%% a comment %%";
        let root = parse(text, Dialect::Obsidian);
        let blocks = root["children"].as_array().unwrap();
        let b = blocks[0]["children"].as_array().unwrap();
        let p = &b[0];
        assert_eq!(p["_block_id"], "para-1");
        assert_eq!(p["children"][1]["_wikilink"], "Other Note#^abc");
        assert_eq!(p["children"][1]["children"][0]["value"], "that");
        assert_eq!(p["children"][3]["_wikilink"], "Plain");
        assert!(p["children"][4]["value"].as_str().unwrap().contains("![[embed.png]]"), "embeds stay text");
        assert_eq!((b[1]["type"].as_str(), b[1]["kind"].as_str()), (Some("proof"), Some("theorem")));
        assert_eq!(b[2]["_block_id"], "code-id");
        assert_eq!(blocks[1]["children"][0], json!({ "type": "mystComment", "value": "a comment" }));
    }

    #[test]
    fn frontmatter_and_references() {
        let root = parse("---\ntitle: A\ntags: [x, y]\nn: 2\n---\n\nSee [the site][s].\n\n[s]: https://example.org \"Ex\"\n", Dialect::Myst);
        assert_eq!(root["frontmatter"], json!({ "title": "A", "tags": ["x", "y"], "n": 2 }));
        assert_eq!(root["children"][0]["children"][1], json!({ "type": "link", "url": "https://example.org", "title": "Ex", "children": [{ "type": "text", "value": "the site" }] }));
    }
}
