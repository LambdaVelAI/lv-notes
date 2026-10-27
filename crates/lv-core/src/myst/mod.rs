//! MyST AST: the document contract.
//!
//! The JSON shape follows the MyST spec (mdast plus MyST extensions) as
//! emitted by `mystmd`. Markdown is import/export only; this AST is what the
//! op log and projections work with.

mod ast;

pub use ast::*;
