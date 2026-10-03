//! Native retrieval. The original exact-symbol engine stays byte-identical for A/B.
pub(crate) mod lexical {
    include!("context.rs");
    pub(crate) mod semantic {
        include!("polaris.rs");
        include!("polaris_scan.rs");
    }
}
pub mod context {
    pub use super::lexical::{code_map, set_data_root};
    pub use super::lexical::semantic::{fast_context, polaris};
}
