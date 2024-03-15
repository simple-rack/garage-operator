use std::fmt::Display;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod access_key;
mod bucket;
mod garage;

pub use access_key::*;
pub use bucket::*;
pub use garage::*;

/// Reference to a namespaced object
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamespacedReference {
    /// The name of the resource
    pub name: String,

    /// The containing namespace.
    pub namespace: String,
}

impl Display for NamespacedReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}
