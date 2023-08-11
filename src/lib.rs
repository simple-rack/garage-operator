use thiserror::Error;

/// Expose all controller components used by main
pub mod resources;

// Internal only
mod garage_admin;

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
    KubeError(#[source] kube::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("IllegalGarage")]
    IllegalGarage,
    #[error("IllegalBucket")]
    IllegalBucket,

    #[error("Network error: {0}")]
    NetworkError(#[from] progenitor_client::Error),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}
