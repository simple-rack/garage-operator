use async_trait::async_trait;
use kube::{runtime::controller::Action, Client, CustomResourceExt, ResourceExt};
use tokio::sync::RwLock;

pub mod garage;

use std::sync::Arc;

use crate::{operator::Diagnostics, Error, Metrics};

/// The context passed around reconcilers
pub(crate) struct Context {
    /// Kubernetes client
    pub client: Client,

    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,

    /// Prometheus metrics
    pub metrics: Metrics,

    /// The version of garage in use
    pub garage_version: String,
}

/// A resource that can be reconciled by a controller
#[async_trait]
pub(crate) trait Reconcile
where
    Self: CustomResourceExt + ResourceExt
{
    /// Attempt to reconcile a resource
    async fn reconcile(&self, context: Arc<Context>) -> Result<Action, Error>;

    /// Attempt to deploy all necessary sub-resources for this CRD.
    async fn deploy_resources(&self, context: Arc<Context>) -> Result<(), Error>;
}
