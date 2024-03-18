use thiserror::Error;

pub mod operator;
pub mod reconcilers;

/// Expose all controller components used by main
pub mod resources;

mod admin_api;

/// Log and trace integrations
pub mod telemetry;

/// Metrics
mod metrics;
pub use metrics::Metrics;

pub const GARAGE_VERSION: &str = "0.8.2";

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("invalid configuration for garage '{0}': {1}")]
    IllegalGarage(String, String),

    #[error("invalid configuration for bucket '{0}': {1}")]
    IllegalBucket(String, String),

    #[error("specified source does not exist: {0}")]
    MissingDataSource(String),

    #[error("specified secret is missing '{0}'")]
    MissingSecret(String),

    #[error("specified secret is missing data '{0}'")]
    MissingSecretData(String),

    #[error("Network error: {0}")]
    NetworkError(#[from] progenitor_client::Error),
}

/// Alias for the common error type
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

/// Create a meta structure for resources with common options
macro_rules! meta {
    (owners: $owners:expr) => {{
        ::kube::core::ObjectMeta {
            owner_references: Some($owner),

            ..Default::default()
        }
    }};

    (owners: $owners:expr, $($lhs:ident : $rhs:expr),*) => {{
        ::kube::core::ObjectMeta {
            owner_references: Some($owners),
            $($lhs : $rhs),*,

            ..Default::default()
        }
    }};
}
pub(crate) use meta;

/// Create common labels for resources managed by garage-operator
macro_rules! labels {
    (instance: $name:expr) => {{
        ::std::collections::BTreeMap::from_iter([
            ("app.kubernetes.io/name".into(), $name),
            ("app.kubernetes.io/version".into(), crate::GARAGE_VERSION.into()),
        ])
    }};

    (instance: $name:expr, $($lhs:ident : $rhs:expr),*) => {{
        ::std::collections::BTreeMap::from_iter([
            ("app.kubernetes.io/name".into(), $name),
            ("app.kubernetes.io/version".into(), crate::GARAGE_VERSION.into()),
            $(($lhs, $rhs))*,
        ])
    }};
}
pub(crate) use labels;
