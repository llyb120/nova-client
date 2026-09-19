//! Generic Operator. The runtime has no dependency on any coding-agent loop.
pub mod core;
pub(crate) mod executors;
mod runtime;
pub use runtime::{
    alias, check_access, execute, scope_for, scope_for_alias, set_parent_alive,
    set_parent_cancelled, tool_definition, Decide, DecisionInput, Registration, SYSTEM,
};
use std::{path::PathBuf, sync::Arc};

pub fn register(
    key: &str,
    root: PathBuf,
    data_root: PathBuf,
    model: core::ModelIdentity,
    decide: Decide,
) -> Registration {
    runtime::register(
        key,
        root,
        data_root,
        model,
        decide,
        runtime::Native {
            chrome: crate::chrome_browser::tool_definition(),
            jianlai: crate::jianlai::tool_definition(),
            execute: Arc::new(|channel, root, args, owner| {
                Box::pin(async move {
                    if channel == "chrome" {
                        crate::native_browser::execute_chrome(&root, &args, &owner).await
                    } else {
                        crate::jianlai::execute(&root, &args, &owner).await
                    }
                })
            }),
        },
    )
}
