use actix_web::{
    get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use prometheus::{Encoder, TextEncoder};

use garage_operator::telemetry;

use crate::controller::State;

#[get("/metrics")]
async fn metrics(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let metrics = c.metrics();
    let encoder = TextEncoder::new();
    let mut buffer = vec![];
    encoder.encode(&metrics, &mut buffer).unwrap();
    HttpResponse::Ok().body(buffer)
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let d = c.diagnostics().await;
    HttpResponse::Ok().json(&d)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;

    // Initiatilize Kubernetes controller state
    let state = State::default();
    let controller = controller::run(state.clone());

    // Start web server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(state.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
            .service(metrics)
    })
    .bind("0.0.0.0:8080")?
    .shutdown_timeout(5);

    // Both runtimes implements graceful shutdown, so poll until both are done
    tokio::join!(controller, server.run()).1?;
    Ok(())
}

mod controller {
    use std::{sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use futures::StreamExt;
    use garage_operator::{
        resources::{AccessKey, Bucket, Garage, GarageStatus},
        telemetry, Error, Metrics, Result,
    };
    use kube::{
        api::{ListParams, Patch, PatchParams},
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
    use serde_json::json;
    use tokio::sync::RwLock;
    use tracing::{error, field, info, instrument, warn, Span};

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
        pub fn to_context(&self, client: Client) -> Arc<Context> {
            Arc::new(Context {
                client,
                metrics: Metrics::default().register(&self.registry).unwrap(),
                diagnostics: self.diagnostics.clone(),
            })
        }
    }

    // Context for our reconciler
    pub struct Context {
        /// Kubernetes client
        pub client: Client,
        /// Diagnostics read by the web server
        pub diagnostics: Arc<RwLock<Diagnostics>>,
        /// Prometheus metrics
        pub metrics: Metrics,
    }

    #[instrument(skip(ctx, garage), fields(trace_id))]
    async fn reconcile(garage: Arc<Garage>, ctx: Arc<Context>) -> Result<Action> {
        let trace_id = telemetry::get_trace_id();
        Span::current().record("trace_id", &field::display(&trace_id));
        let _timer = ctx.metrics.count_and_measure();
        ctx.diagnostics.write().await.last_event = Utc::now();
        let ns = garage.namespace().unwrap(); // garage is namespace scoped
        let garages: Api<Garage> = Api::namespaced(ctx.client.clone(), &ns);

        info!("Reconciling Garage \"{}\" in {}", garage.name_any(), ns);
        finalizer(&garages, GARAGE_FINALIZER, garage, |event| async {
            match event {
                Finalizer::Apply(g) => reconcile_garage(g, ctx.clone()).await,
                Finalizer::Cleanup(g) => cleanup(g, ctx.clone()).await,
            }
        })
        .await
        .map_err(|e| Error::FinalizerError(Box::new(e)))
    }

    fn error_policy(garage: Arc<Garage>, error: &Error, ctx: Arc<Context>) -> Action {
        warn!("reconcile failed: {:?}", error);
        ctx.metrics.reconcile_failure(&garage, error);
        Action::requeue(Duration::from_secs(5 * 60))
    }

    /// Initialize the controller and shared state (given the crd is installed)
    pub async fn run(state: State) {
        let client = Client::try_default()
            .await
            .expect("failed to create kube Client");
        let garages = Api::<Garage>::all(client.clone());
        let buckets = Api::<Bucket>::all(client.clone());
        let access_keys = Api::<AccessKey>::all(client.clone());

        if let Err(e) = garages.list(&ListParams::default().limit(1)).await {
            error!("CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }
        Controller::new(garages, Config::default().any_semantic())
            .shutdown_on_signal()
            .watches(buckets, Config::default().any_semantic(), |bucket| {
                // If we don't know how to get the namespace / instance, then do nothing
                // TODO: Warn here or something
                let Some((namespace, instance)) = bucket.spec.garage_ref.split_once("/") else {
                    return vec![].into_iter();
                };

                vec![ObjectRef::<Garage>::new(instance).within(namespace)].into_iter()
            })
            .watches(access_keys, Config::default().any_semantic(), |access_key| {
                // If we don't know how to get the namespace / instance, then do nothing
                // TODO: Warn here or something
                let Some((namespace, instance)) = access_key.spec.garage_ref.split_once("/") else {
                    return vec![].into_iter();
                };

                vec![ObjectRef::<Garage>::new(instance).within(namespace)].into_iter()
            })
            .run(reconcile, error_policy, state.to_context(client))
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(()))
            .await;
    }

    async fn handle_transition(
        garage: Arc<Garage>,
        ctx: Arc<Context>,
        from_state: GarageStatus,
    ) -> Result<GarageStatus> {
        let should_layout = garage.spec.autolayout;
        let recorder = ctx
            .diagnostics
            .read()
            .await
            .recorder(ctx.client.clone(), &garage);
        let name = garage.name_any();
        let ns = garage.namespace().clone().unwrap_or("default".into());

        let buckets = Api::<Bucket>::all(ctx.client.clone());
        let owned_buckets = buckets
            .list(&Default::default())
            .await
            .map_err(Error::KubeError)?;

        Ok(match from_state {
            GarageStatus::Creating => {
                // Deploy the garage instance
                garage.deploy_resources(ctx.client.clone()).await?;

                if should_layout {
                    GarageStatus::LayingOut
                } else {
                    GarageStatus::Ready
                }
            }
            GarageStatus::LayingOut => {
                // Give the service time to finish starting up
                // TODO: Is this nasty? Returning an action won't help since it
                //   only is taken into consideration when events occur, I think...
                //   Maybe we have the operator check that the service is healthy
                //   in the layout step before actually attempting to use the
                //   admin? Or we could have the admin try an x amount of times
                //   before actually failing...
                tokio::time::sleep(Duration::from_secs(5)).await;

                // send an event once per layout request
                recorder
                    .publish(Event {
                        type_: EventType::Normal,
                        reason: "LayoutRequested".into(),
                        note: Some(format!("Configuring layout for `{name}`")),
                        action: "Layout".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;

                // Actually layout the instance
                let admin = garage.create_admin(ctx.client.clone()).await?;
                admin.layout_instance().await?;

                GarageStatus::Ready
            }
            GarageStatus::Ready => {
                let buckets: Vec<_> = owned_buckets
                    .into_iter()
                    .filter(|bucket| bucket.spec.garage_ref == format!("{ns}/{name}"))
                    .collect();

                garage.handle_buckets(ctx.client.clone(), &buckets).await?;

                GarageStatus::Ready
            }
        })
    }

    // Reconcile (for non-finalizer related changes)
    async fn reconcile_garage(garage: Arc<Garage>, ctx: Arc<Context>) -> Result<Action> {
        let client = ctx.client.clone();
        let ns = garage.namespace().unwrap();
        let name = garage.name_any();
        let garages: Api<Garage> = Api::namespaced(client, &ns);

        // See what the next state should be
        let status = garage.status.clone().unwrap_or_default();
        let status = handle_transition(garage, ctx.clone(), status).await?;

        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "deuxfleurs.fr/v0alpha",
            "kind": "Garage",
            "status": status,
        }));
        let ps = PatchParams::apply("garage-operator").force();
        let _o = garages
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;

        // If no events were received, check back every 5 minutes
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

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
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
}
