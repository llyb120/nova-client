//! Native retrieval. Preserve the exact-symbol engine while sharing its scanner with
//! the natural-language pipeline. The small facade is the only public tool boundary.
// include! keeps the established engine byte-identical for regression/A-B comparison;
// the descendant module can reuse its parser without widening private internals.
pub(crate) mod lexical {
    include!("context.rs");
    pub(crate) mod semantic { include!("polaris.rs"); }
}
pub mod context {
    pub use super::lexical::{code_map, set_data_root};
    pub use super::lexical::semantic::{fast_context, polaris};
}
