//! The project schema registry (invariant 8).
//!
//! A project declares its own vocabulary as data, changed by ops like any
//! other content: document kinds (`note`), block kinds (`proof:theorem`,
//! `admonition:warning`) and link relations (`supports`). See
//! `docs/design/registry.md`.
//!
//! Every op that introduces a kind or relation is checked here, in one
//! place, by [`ProjectState::apply`](crate::state::ProjectState::apply):
//! clients ask, the core decides. Only *new* uses are checked: content that
//! keeps the kind it already had stays valid even if that kind was retired,
//! or predates the registry.
//!
//! A project with no declarations at all is not checked: its log predates
//! the registry. Apps declare [`builtins`] (see [`bootstrap`]) when they
//! create or first open a project, and checking starts from there.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::op::{NodeContent, OpBody, Relation};
use crate::refusal::{Refusal, RefusalCode as Code};
use crate::state::ProjectState;

/// Which vocabulary a name belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Namespace {
    /// Document kinds: `note`, `draft`.
    Document,
    /// `<node type>:<kind>` for the node types in [`KINDED_TYPES`].
    Block,
    /// Link relations: `supports`, `contradicts`.
    Relation,
}

impl Namespace {
    pub fn as_str(&self) -> &'static str {
        match self {
            Namespace::Document => "document",
            Namespace::Block => "block",
            Namespace::Relation => "relation",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindStatus {
    /// In normal use.
    Declared,
    /// Arrived with imported content; usable, but flagged for review.
    Provisional,
    /// Existing uses stay valid; new uses are refused.
    Retired,
}

impl KindStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            KindStatus::Declared => "declared",
            KindStatus::Provisional => "provisional",
            KindStatus::Retired => "retired",
        }
    }
}

/// How a document kind lays out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Reflowing text: notes, articles.
    Flow,
    /// A sequence of fixed-aspect frames: decks.
    Canvas,
}

/// One registered name. Fields beyond `label` and `status` belong to one
/// namespace each; adding optional fields later needs no migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindEntry {
    /// Shown in UIs and the agent guide: "Theorem".
    pub label: String,
    pub status: KindStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Document kinds only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Layout>,
    /// Block kinds only: the numbering counter, shared by kinds that count
    /// together (Theorem 1, Lemma 2). `None` is unnumbered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<String>,
    /// Relations only: how the relation reads from its target ("supported by").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse_label: Option<String>,
}

impl KindEntry {
    pub fn new(label: impl Into<String>) -> KindEntry {
        KindEntry {
            label: label.into(),
            status: KindStatus::Declared,
            description: None,
            layout: None,
            counter: None,
            inverse_label: None,
        }
    }

    /// Checks the entry's fields suit `namespace`.
    pub(crate) fn check(&self, namespace: Namespace) -> Result<(), Refusal> {
        let wrong = match namespace {
            Namespace::Document => self.counter.is_some() || self.inverse_label.is_some(),
            Namespace::Block => self.layout.is_some() || self.inverse_label.is_some(),
            Namespace::Relation => self.layout.is_some() || self.counter.is_some(),
        };
        if wrong {
            return Err(Refusal::new(
                Code::InvalidOp,
                format!("{} entries take only their own fields (layout: documents, counter: blocks, inverse_label: relations)", namespace.as_str()),
            ));
        }
        if self.label.trim().is_empty() {
            return Err(Refusal::new(Code::InvalidOp, "a kind needs a label"));
        }
        Ok(())
    }
}

/// Node types whose `kind` field names a block kind.
pub const KINDED_TYPES: [&str; 3] = ["proof", "admonition", "container"];

/// The project's vocabulary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registry {
    entries: BTreeMap<(Namespace, String), KindEntry>,
}

impl Registry {
    pub fn get(&self, namespace: Namespace, name: &str) -> Option<&KindEntry> {
        self.entries.get(&(namespace, name.to_owned()))
    }

    /// Every entry, by namespace then name.
    pub fn entries(&self) -> impl Iterator<Item = (Namespace, &str, &KindEntry)> {
        self.entries.iter().map(|((ns, name), e)| (*ns, name.as_str(), e))
    }

    /// Whether ops are checked: the project has declared anything at all.
    pub fn is_enforced(&self) -> bool {
        !self.entries.is_empty()
    }

    pub(crate) fn insert(&mut self, namespace: Namespace, name: &str, entry: KindEntry) {
        self.entries.insert((namespace, name.to_owned()), entry);
    }

    pub(crate) fn remove(&mut self, namespace: Namespace, name: &str) -> Option<KindEntry> {
        self.entries.remove(&(namespace, name.to_owned()))
    }

    /// Checks a new use of `name`.
    pub fn check_use(&self, namespace: Namespace, name: &str) -> Result<(), Refusal> {
        if !self.is_enforced() {
            return Ok(());
        }
        match self.get(namespace, name) {
            None => Err(Refusal::new(
                Code::UndeclaredKind,
                format!("`{name}` is not a declared {} kind; declare it first, or use a declared one", namespace.as_str()),
            )),
            Some(e) if e.status == KindStatus::Retired => {
                Err(Refusal::new(Code::RetiredKind, format!("`{name}` is retired; existing uses stay, new ones are refused")))
            }
            Some(_) => Ok(()),
        }
    }

    /// Checks `new` content replacing `old` (`None` for a new node): its
    /// block kind, if it has one and it changed.
    pub fn check_content(&self, old: Option<&NodeContent>, new: &NodeContent) -> Result<(), Refusal> {
        match block_kind(new) {
            Some(kind) if old.and_then(block_kind).as_ref() != Some(&kind) => self.check_use(Namespace::Block, &kind),
            _ => Ok(()),
        }
    }

    /// Checks a link's relation changing from `old` to `new`.
    pub fn check_relation(&self, old: Option<&Relation>, new: Option<&Relation>) -> Result<(), Refusal> {
        match new {
            Some(r) if old != Some(r) => self.check_use(Namespace::Relation, &r.0),
            _ => Ok(()),
        }
    }
}

/// The block-kind name of `content`, e.g. `proof:theorem`, if its type is
/// in [`KINDED_TYPES`] and it has a `kind`.
pub fn block_kind(content: &NodeContent) -> Option<String> {
    if !KINDED_TYPES.contains(&content.node_type.as_str()) {
        return None;
    }
    let kind = content.fields.get("kind").and_then(Value::as_str)?;
    Some(format!("{}:{kind}", content.node_type))
}

/// Checks a name is well formed for its namespace.
pub(crate) fn check_name(namespace: Namespace, name: &str) -> Result<(), Refusal> {
    let bad = |why: &str| Err(Refusal::new(Code::InvalidOp, format!("`{name}` is not a valid {} name: {why}", namespace.as_str())));
    if name.trim().is_empty() || name.trim() != name {
        return bad("empty or padded");
    }
    if namespace == Namespace::Block {
        match name.split_once(':') {
            Some((t, k)) if KINDED_TYPES.contains(&t) && !k.is_empty() => {}
            _ => return bad("block kinds are `<node type>:<kind>`, with a type of proof, admonition or container"),
        }
    }
    Ok(())
}

/// The node type part of a block-kind name.
pub(crate) fn block_type(name: &str) -> &str {
    name.split_once(':').map_or(name, |(t, _)| t)
}

/// Built-in vocabulary: declared in each project's own log, so a copy of
/// the project carries it.
pub fn builtins() -> Vec<OpBody> {
    let mut ops = Vec::new();
    let mut declare = |namespace, name: &str, entry: KindEntry| {
        ops.push(OpBody::DeclareKind { namespace, name: name.to_owned(), entry });
    };
    for (name, label) in [("note", "Note"), ("draft", "Draft")] {
        declare(Namespace::Document, name, KindEntry { layout: Some(Layout::Flow), ..KindEntry::new(label) });
    }
    // MyST's `prf:` kinds; the theorem-like ones share a counter.
    for (kind, label, counter) in [
        ("theorem", "Theorem", Some("theorem")),
        ("lemma", "Lemma", Some("theorem")),
        ("corollary", "Corollary", Some("theorem")),
        ("proposition", "Proposition", Some("theorem")),
        ("conjecture", "Conjecture", Some("theorem")),
        ("definition", "Definition", Some("definition")),
        ("axiom", "Axiom", Some("axiom")),
        ("assumption", "Assumption", Some("assumption")),
        ("example", "Example", Some("example")),
        ("remark", "Remark", Some("remark")),
        ("observation", "Observation", Some("observation")),
        ("property", "Property", Some("property")),
        ("criterion", "Criterion", Some("criterion")),
        ("notation", "Notation", Some("notation")),
        ("algorithm", "Algorithm", Some("algorithm")),
        ("proof", "Proof", None),
    ] {
        declare(Namespace::Block, &format!("proof:{kind}"), KindEntry { counter: counter.map(str::to_owned), ..KindEntry::new(label) });
    }
    for (kind, label) in [
        ("note", "Note"),
        ("tip", "Tip"),
        ("hint", "Hint"),
        ("important", "Important"),
        ("warning", "Warning"),
        ("caution", "Caution"),
        ("attention", "Attention"),
        ("danger", "Danger"),
        ("error", "Error"),
        ("seealso", "See also"),
        ("admonition", "Admonition"),
    ] {
        declare(Namespace::Block, &format!("admonition:{kind}"), KindEntry::new(label));
    }
    for (kind, label) in [("figure", "Figure"), ("table", "Table"), ("code", "Code")] {
        declare(Namespace::Block, &format!("container:{kind}"), KindEntry { counter: Some(kind.to_owned()), ..KindEntry::new(label) });
    }
    for (name, label, inverse) in [
        ("supports", "Supports", "Supported by"),
        ("contradicts", "Contradicts", "Contradicted by"),
        ("extends", "Extends", "Extended by"),
        ("uses", "Uses", "Used by"),
        ("proves", "Proves", "Proved by"),
        ("cites", "Cites", "Cited by"),
        ("defines", "Defines", "Defined by"),
        ("example-of", "Example of", "Has example"),
    ] {
        declare(Namespace::Relation, name, KindEntry { inverse_label: Some(inverse.to_owned()), ..KindEntry::new(label) });
    }
    ops
}

/// What a project with no declarations needs to start checking: the
/// built-ins, plus every name its content already uses that isn't one,
/// declared `Provisional` so existing content stays valid. Empty if the
/// project already has a registry.
pub fn bootstrap(state: &ProjectState) -> Vec<OpBody> {
    if state.registry().is_enforced() {
        return vec![];
    }
    let mut ops = builtins();
    let known: BTreeSet<(Namespace, String)> = ops
        .iter()
        .filter_map(|op| match op {
            OpBody::DeclareKind { namespace, name, .. } => Some((*namespace, name.clone())),
            _ => None,
        })
        .collect();
    let mut used = BTreeSet::new();
    for (_, d) in state.documents() {
        used.insert((Namespace::Document, d.kind.0.clone()));
    }
    for (_, n) in state.nodes() {
        if let Some(kind) = block_kind(&n.content) {
            used.insert((Namespace::Block, kind));
        }
        if let Some(r) = &n.relation {
            used.insert((Namespace::Relation, r.0.clone()));
        }
    }
    ops.extend(used.difference(&known).filter(|(ns, name)| check_name(*ns, name).is_ok()).map(|(ns, name)| provisional(*ns, name)));
    ops
}

/// Declarations for the names `ops` use that the registry doesn't know, as
/// `Provisional`: what makes an import lenient. Empty while the registry is
/// not enforced.
pub fn declare_unknown(state: &ProjectState, ops: &[OpBody]) -> Vec<OpBody> {
    let registry = state.registry();
    if !registry.is_enforced() {
        return vec![];
    }
    let mut missing = BTreeSet::new();
    for op in ops {
        let used = match op {
            OpBody::CreateDocument { kind, .. } => Some((Namespace::Document, kind.0.clone())),
            OpBody::InsertNode { content, .. } => block_kind(content).map(|k| (Namespace::Block, k)),
            OpBody::SetRelation { relation: Some(r), .. } => Some((Namespace::Relation, r.0.clone())),
            _ => None,
        };
        if let Some((ns, name)) = used
            && registry.get(ns, &name).is_none()
            && check_name(ns, &name).is_ok()
        {
            missing.insert((ns, name));
        }
    }
    missing.into_iter().map(|(ns, name)| provisional(ns, &name)).collect()
}

fn provisional(namespace: Namespace, name: &str) -> OpBody {
    let label = name.rsplit(':').next().unwrap_or(name).replace(['-', '_'], " ");
    let mut label: Vec<char> = label.chars().collect();
    if let Some(c) = label.first_mut() {
        *c = c.to_ascii_uppercase();
    }
    let mut entry = KindEntry::new(label.into_iter().collect::<String>());
    entry.status = KindStatus::Provisional;
    if namespace == Namespace::Document {
        entry.layout = Some(Layout::Flow);
    }
    OpBody::DeclareKind { namespace, name: name.to_owned(), entry }
}
