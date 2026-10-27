//! Typed MyST AST nodes with lossless JSON round-tripping.
//!
//! Each known node type is a struct. Fields this model does not name are
//! kept in `extra`, and node types it does not know become
//! [`Node::Unknown`], so any MyST JSON survives a parse and re-serialize
//! unchanged. Optional spec fields are `Option`s so that "absent" and
//! "present with a default value" stay distinguishable.

use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::ids::NodeId;

/// Fields not modelled explicitly on a node, preserved verbatim.
pub type Extra = Map<String, Value>;

/// A point in the source text (1-based line and column).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// Source span of a node, when it came from parsed text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub start: Point,
    pub end: Point,
}

/// Fields any MyST node may carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Common {
    /// Stable lv-notes ID. Present on blocks and anchor-like inline nodes
    /// once they are in a project; Markdown import assigns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NodeId>,
    /// Normalized reference target (lowercased label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// Label as written by the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

/// Declares a node struct. Every node gets `common` and `extra`. Fields in
/// the first group are always serialized; fields after `?` are `Option`s,
/// omitted when absent.
macro_rules! node_struct {
    (
        $(#[$m:meta])* $name:ident {
            $($req:ident : $rty:ty),* $(,)?
        }
        $(? { $($opt:ident : $oty:ty),* $(,)? })?
    ) => {
        $(#[$m])*
        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            $(
                #[serde(default)]
                pub $req: $rty,
            )*
            $($(
                #[serde(default, skip_serializing_if = "Option::is_none")]
                pub $opt: Option<$oty>,
            )*)?
            #[serde(flatten)]
            pub common: Common,
            #[serde(flatten)]
            pub extra: Extra,
        }
    };
}

// ---- Structure ----------------------------------------------------------

node_struct!(/// The document root.
    Root { children: Vec<Node> });
node_struct!(/// A MyST block (`+++` separated), the coarse unit of a document.
    Block { children: Vec<Node> } ? { meta: String });
node_struct!(Paragraph { children: Vec<Node> });
node_struct!(Heading { depth: u8, children: Vec<Node> } ? { enumerated: bool, enumerator: String });
node_struct!(Blockquote { children: Vec<Node> });
node_struct!(List { children: Vec<Node> } ? { ordered: bool, start: u64, spread: bool });
node_struct!(ListItem { children: Vec<Node> } ? { spread: bool, checked: bool });
node_struct!(ThematicBreak {});
node_struct!(Code { value: String } ? { lang: String, meta: String });
node_struct!(Math { value: String } ? { enumerated: bool, enumerator: String });
node_struct!(Html { value: String });
node_struct!(/// A figure, table or code container with optional caption.
    Container { kind: String, children: Vec<Node> } ? { enumerated: bool, enumerator: String });
node_struct!(Caption { children: Vec<Node> });
node_struct!(Legend { children: Vec<Node> });
node_struct!(Admonition { children: Vec<Node> } ? { kind: String, class: String });
node_struct!(AdmonitionTitle { children: Vec<Node> });
node_struct!(Table { children: Vec<Node> } ? { align: Vec<Option<String>> });
node_struct!(TableRow { children: Vec<Node> });
node_struct!(TableCell { children: Vec<Node> } ? { header: bool, align: String });
node_struct!(FootnoteDefinition { children: Vec<Node> });

// ---- Inline -------------------------------------------------------------

node_struct!(Text { value: String });
node_struct!(Emphasis { children: Vec<Node> });
node_struct!(Strong { children: Vec<Node> });
node_struct!(Underline { children: Vec<Node> });
node_struct!(Subscript { children: Vec<Node> });
node_struct!(Superscript { children: Vec<Node> });
node_struct!(Delete { children: Vec<Node> });
node_struct!(InlineCode { value: String });
node_struct!(InlineMath { value: String });
node_struct!(Break {});
node_struct!(Link { url: String, children: Vec<Node> } ? { title: String });
node_struct!(Image { url: String } ? { alt: String, title: String, width: String, align: String });
node_struct!(/// A reference to a labelled target (`{ref}`, `{numref}`, `[](#x)`).
    CrossReference { children: Vec<Node> } ? { kind: String });
node_struct!(/// A group of citations, `parenthetical` or `narrative`.
    CiteGroup { kind: String, children: Vec<Node> });
node_struct!(/// A single citation; the citation key is in `common.label`.
    Cite {} ? { kind: String, prefix: String, suffix: String, children: Vec<Node> });
node_struct!(FootnoteReference {});

// ---- MyST syntax nodes --------------------------------------------------

node_struct!(/// A directive as written (`:::{name} args`), before or after expansion.
    MystDirective { name: String } ? {
        args: String,
        options: Map<String, Value>,
        value: String,
        children: Vec<Node>,
    });
node_struct!(/// A role as written (`` {name}`value` ``).
    MystRole { name: String } ? { value: String, children: Vec<Node> });
node_struct!(/// A label target (`(label)=`); the label is in `common.label`.
    MystTarget {});
node_struct!(MystComment { value: String });

// ---- The node enum ------------------------------------------------------

macro_rules! nodes {
    (
        parents { $($p:ident = $pt:literal,)* }
        optional_parents { $($o:ident = $ot:literal,)* }
        leaves { $($l:ident = $lt:literal,)* }
    ) => {
        /// Any MyST AST node, tagged by its JSON `type`.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Node {
            $($p($p),)*
            $($o($o),)*
            $($l($l),)*
            /// A node type this model does not know, kept as raw JSON
            /// (including its `type` field).
            Unknown(Map<String, Value>),
        }

        impl Node {
            /// The MyST `type` string.
            pub fn type_name(&self) -> &str {
                match self {
                    $(Node::$p(_) => $pt,)*
                    $(Node::$o(_) => $ot,)*
                    $(Node::$l(_) => $lt,)*
                    Node::Unknown(m) => m.get("type").and_then(Value::as_str).unwrap_or(""),
                }
            }

            /// Typed children. Empty for leaves and for [`Node::Unknown`].
            pub fn children(&self) -> &[Node] {
                match self {
                    $(Node::$p(n) => &n.children,)*
                    $(Node::$o(n) => n.children.as_deref().unwrap_or(&[]),)*
                    $(Node::$l(_) => &[],)*
                    Node::Unknown(_) => &[],
                }
            }

            /// Fields shared by all node types. `None` for [`Node::Unknown`].
            pub fn common(&self) -> Option<&Common> {
                match self {
                    $(Node::$p(n) => Some(&n.common),)*
                    $(Node::$o(n) => Some(&n.common),)*
                    $(Node::$l(n) => Some(&n.common),)*
                    Node::Unknown(_) => None,
                }
            }
        }

        impl Serialize for Node {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let (tag, value) = match self {
                    $(Node::$p(n) => ($pt, serde_json::to_value(n)),)*
                    $(Node::$o(n) => ($ot, serde_json::to_value(n)),)*
                    $(Node::$l(n) => ($lt, serde_json::to_value(n)),)*
                    Node::Unknown(m) => return m.serialize(serializer),
                };
                let Value::Object(mut map) = value.map_err(S::Error::custom)? else {
                    return Err(S::Error::custom("node did not serialize to an object"));
                };
                map.insert("type".to_owned(), Value::String(tag.to_owned()));
                map.serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for Node {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let map = Map::<String, Value>::deserialize(deserializer)?;
                let tag = match map.get("type") {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err(D::Error::custom("MyST node without a string `type`")),
                };
                fn typed<T: serde::de::DeserializeOwned, E: serde::de::Error>(
                    mut map: Map<String, Value>,
                ) -> Result<T, E> {
                    map.remove("type");
                    serde_json::from_value(Value::Object(map)).map_err(E::custom)
                }
                Ok(match tag.as_str() {
                    $($pt => Node::$p(typed::<_, D::Error>(map)?),)*
                    $($ot => Node::$o(typed::<_, D::Error>(map)?),)*
                    $($lt => Node::$l(typed::<_, D::Error>(map)?),)*
                    _ => Node::Unknown(map),
                })
            }
        }
    };
}

nodes! {
    parents {
        Root = "root",
        Block = "block",
        Paragraph = "paragraph",
        Heading = "heading",
        Blockquote = "blockquote",
        List = "list",
        ListItem = "listItem",
        Container = "container",
        Caption = "caption",
        Legend = "legend",
        Admonition = "admonition",
        AdmonitionTitle = "admonitionTitle",
        Table = "table",
        TableRow = "tableRow",
        TableCell = "tableCell",
        FootnoteDefinition = "footnoteDefinition",
        Emphasis = "emphasis",
        Strong = "strong",
        Underline = "underline",
        Subscript = "subscript",
        Superscript = "superscript",
        Delete = "delete",
        Link = "link",
        CrossReference = "crossReference",
        CiteGroup = "citeGroup",
    }
    optional_parents {
        Cite = "cite",
        MystDirective = "mystDirective",
        MystRole = "mystRole",
    }
    leaves {
        ThematicBreak = "thematicBreak",
        Code = "code",
        Math = "math",
        Html = "html",
        Text = "text",
        InlineCode = "inlineCode",
        InlineMath = "inlineMath",
        Break = "break",
        Image = "image",
        FootnoteReference = "footnoteReference",
        MystTarget = "mystTarget",
        MystComment = "mystComment",
    }
}

impl Node {
    /// Depth-first iterator over this node and all its typed descendants.
    pub fn walk(&self) -> impl Iterator<Item = &Node> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            stack.extend(node.children().iter().rev());
            Some(node)
        })
    }
}
