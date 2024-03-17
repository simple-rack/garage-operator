use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    api::ListParams,
    core::object::HasSpec,
    runtime::{
        controller::Action,
        events::{Event, EventType, Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        reflector::ObjectRef,
        watcher::Config,
        Controller,
    },
    Api, Client, Resource, ResourceExt,
};
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{error, field, info, instrument, Span};

use crate::{
    reconcilers::{Context, Reconcile},
    resources::{AccessKey, Bucket, Garage},
    telemetry, Error, Metrics, Result,
};

pub const GARAGE_FINALIZER: &str = "garage.deuxfleurs.fr";

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "garage-operator".into(),
        }
    }
}
impl Diagnostics {
    pub fn recorder(&self, client: Client, garage: &Garage) -> Recorder {
        Recorder::new(client, self.reporter.clone(), garage.object_ref(&()))
    }
}

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics populated by the reconciler
    diagnostics: Arc<RwLock<Diagnostics>>,
    /// Metrics registry
    registry: prometheus::Registry,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Metrics getter
    pub fn metrics(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.registry.gather()
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }

    // Create a Controller Context that can update State
    pub(crate) fn to_context(&self, client: Client, garage_version: String) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: Metrics::default().register(&self.registry).unwrap(),
            diagnostics: self.diagnostics.clone(),
            garage_version,
        })
    }
}

pub struct GarageController {
    state: State,
}

impl GarageController {
    pub fn new(state: State) -> Self {
        Self { state }
    }

    /// Initialize the controller and shared state (given the crd is installed)
    pub async fn run(self, garage_version: String) -> Result<(), anyhow::Error> {
        // Error handler for failed reconciliations
        fn error_policy(garage: Arc<Garage>, error: &Error, ctx: Arc<Context>) -> Action {
            error!("reconcile failed: {:?}", error);
            ctx.metrics.reconcile_failure(&garage, error);
            Action::requeue(Duration::from_secs(5))
        }

        // Get a k8s client for communicating with the cluster
        let client = Client::try_default()
            .await
            .expect("failed to create kube Client");

        // Create fetchers to our CRDs
        let garages = Api::<Garage>::all(client.clone());
        let buckets = Api::<Bucket>::all(client.clone());
        let access_keys = Api::<AccessKey>::all(client.clone());

        // Test that we can actually query for our CRDs (a.k.a. they are installed)
        if let Err(e) = garages.list(&ListParams::default().limit(1)).await {
            error!("CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }

        // Create a new k8s controller for our CRD resources
        let watching_config = Config::default().page_size(50).any_semantic();
        Controller::new(garages, watching_config.clone())
            .shutdown_on_signal()
            .watches(buckets, watching_config.clone(), |bucket| {
                // Kick off reconciliation for the owning garage
                Some(
                    ObjectRef::new(&bucket.spec.garage_ref.name)
                        .within(&bucket.spec.garage_ref.namespace),
                )
            })
            .watches(access_keys, watching_config, |access_key| {
                // Kick off reconciliation for the owning garage
                Some(
                    ObjectRef::new(&access_key.spec().garage_ref.name)
                        .within(&access_key.spec().garage_ref.namespace),
                )
            })
            .run(
                reconcile,
                error_policy,
                self.state.to_context(client, garage_version),
            )
            .filter_map(|x| async move { Result::ok(x) })
            .for_each(|_| futures::future::ready(()))
            .await;

        Ok(())
    }
}

/// Main reconciler for all garage operator related resources
#[instrument(skip(ctx, garage), fields(trace_id))]
async fn reconcile(garage: Arc<Garage>, ctx: Arc<Context>) -> Result<Action> {
    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(garage: Arc<Garage>, ctx: Arc<Context>) -> Result<Action> {
        let recorder = ctx
            .diagnostics
            .read()
            .await
            .recorder(ctx.client.clone(), &garage);

        // Garage doesn't have any real cleanup, so we just publish an event
        recorder
            .publish(Event {
                type_: EventType::Normal,
                reason: "DeleteRequested".into(),
                note: Some(format!("Delete `{}`", garage.name_any())),
                action: "Deleting".into(),
                secondary: None,
            })
            .await?;

        Ok(Action::await_change())
    }

    // Add some tracing for debugging's sake
    let trace_id = telemetry::get_trace_id();

    // Take some metrics to see the average reconcile time
    Span::current().record("trace_id", &field::display(&trace_id));
    let _timer = ctx.metrics.count_and_measure();
    ctx.diagnostics.write().await.last_event = Utc::now();

    let garages_handle: Api<Garage> =
        Api::namespaced(ctx.client.clone(), garage.namespace().unwrap().as_str());

    let name = garage.name_any();
    let namespace = garage
        .namespace()
        .ok_or_else(|| Error::IllegalGarage(name.clone(), "missing namespace".into()))?;

    info!(r#"Starting Garage reconciliation for "{namespace}/{name}""#);
    finalizer(&garages_handle, GARAGE_FINALIZER, garage, |event| async {
        match event {
            Finalizer::Apply(g) => g.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(g) => cleanup(g, ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}
