use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use indoc::formatdoc;
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, PersistentVolumeClaim,
            PersistentVolumeClaimVolumeSource, PodSpec, PodTemplateSpec, Secret,
            SecretVolumeSource, Service, ServicePort, ServiceSpec, Volume, VolumeMount,
        },
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{
    api::{ListParams, Patch, PatchParams},
    runtime::controller::Action,
    Api, Resource as _, ResourceExt as _,
};
use kube_quantity::ParsedQuantity;
use serde_json::json;
use tokio::try_join;
use tracing::info;
use uuid::Uuid;

use crate::{
    admin_api::GarageAdmin,
    labels, meta,
    resources::{Bucket, Garage, GarageState},
    Error,
};

use super::{bucket::BucketContext, CommonContext as Context, Reconcile};

#[async_trait]
impl Reconcile for Garage {
    type Context = Context;

    async fn reconcile(&self, context: Arc<Self::Context>) -> Result<Action, Error> {
        // Extract needed info from this garage
        let name = self.name_any();
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalGarage(name.clone(), "missing namespace".into()))?;

        // API handles
        let garage_handle: Api<Garage> = Api::namespaced(context.client.clone(), &namespace);
        let bucket_handle: Api<Bucket> = Api::all(context.client.clone());

        // Get the last known status of this garage, using the default if not present
        let status = self.status.clone().unwrap_or_default();

        // Always deploy all of the needed resources, as they are idempotent
        self.deploy_resources(context.clone()).await?;

        // Handle what we need for now
        let (requeue, next_state): (Duration, GarageState) = match status.state {
            // If we need to create the instance, then do so now
            GarageState::Creating => {
                info!(r#"Creating garage "{}/{}"#, namespace, name);

                let next_state = if self.spec.auto_layout {
                    GarageState::LayingOut
                } else {
                    GarageState::Ready
                };

                (Duration::from_secs(2), next_state)
            }

            // If we need to layout the garage instance, then attempt to do so now
            GarageState::LayingOut => {
                // Actually layout the instance
                let admin = self.create_admin(context.clone()).await?;
                let done = admin.layout_instance(status.capacity).await?;

                // Keep trying to layout the server until it completes
                (
                    Duration::from_secs(2),
                    if done {
                        GarageState::Ready
                    } else {
                        GarageState::LayingOut
                    },
                )
            }

            // If we are done and ready, then reconcile the buckets and check again in an hour in case we missed something
            GarageState::Ready => {
                // Get all buckets that we own and reconcile them
                // TODO: Should we do this in parallel?
                // TODO: Listing requires filtering until `selectableFields` is stabilised and added to k8s (v1.30 and beyond)
                let owned_buckets = bucket_handle
                    .list(&ListParams::default())
                    .await?
                    .into_iter()
                    .filter(|b| {
                        b.spec.garage_ref.name == name && b.spec.garage_ref.namespace == namespace
                    });

                let bucket_context = Arc::new(BucketContext {
                    common: context.clone(),
                    owner: self.clone(),
                });
                for bucket in owned_buckets {
                    bucket.reconcile(bucket_context.clone()).await?;
                }

                (Duration::from_secs(60 * 60), GarageState::Ready)
            }

            // If we have encountered an error, try to start over in 15 seconds
            GarageState::Errored => (Duration::from_secs(15), GarageState::Creating),
        };

        // always overwrite status object with what we saw
        let capacity = {
            let caps = self.get_capacities(context.clone()).await?;
            let cap = caps
                .into_iter()
                .fold(ParsedQuantity::default(), |acc, cur| acc + cur);

            cap.to_bytes_i64().unwrap()
        };

        let new_status = Patch::Apply(json!({
            "apiVersion": "deuxfleurs.fr/v0alpha",
            "kind": "Garage",
            "status": {
                "state": next_state,
                "capacity": capacity,
            },
        }));
        let ps = PatchParams::apply("garage-operator").force(); // TODO: Why is this force?
        let _o = garage_handle.patch_status(&name, &ps, &new_status).await?;

        Ok(Action::requeue(requeue))
    }

    async fn deploy_resources(&self, context: Arc<Context>) -> Result<(), Error> {
        // Create all of the dependent resources at once, since they are independent of each other
        try_join!(
            self.create_config(context.clone()),
            self.create_secrets(context.clone()),
            self.create_services(context.clone()),
        )?;

        // Now deploy with the above resources
        self.create_deployment(context).await
    }
}

impl Garage {
    pub async fn create_admin(&self, context: Arc<Context>) -> Result<GarageAdmin, Error> {
        // Fetch the garage admin secret token from k8s
        let token = {
            let namespace = self.namespace().ok_or(Error::IllegalGarage(
                self.name_any(),
                "missing namespace".into(),
            ))?;
            let default_name = self.prefixed_name("admin.key");
            let admin_token_name = self
                .spec
                .secrets
                .admin
                .as_ref()
                .and_then(|a| a.name.as_ref())
                .unwrap_or(&default_name);

            let secrets = Api::<Secret>::namespaced(context.client.clone(), &namespace);

            let secret = secrets
                .get_opt(&admin_token_name)
                .await?
                .ok_or_else(|| Error::MissingSecret(admin_token_name.clone()))?;
            let token = secret
                .data
                .ok_or_else(|| Error::MissingSecretData(admin_token_name.clone()))?;
            let token = token
                .get("key")
                .ok_or_else(|| Error::MissingSecretData(admin_token_name.clone()))?;

            String::from_utf8(token.0.clone()).unwrap()
        };

        // Construct the admin API with our secret
        Ok(GarageAdmin::with_secret(&self, &token)?)
    }

    /// Create a [ConfigMap] for storing the garage's configuration
    async fn create_config(&self, context: Arc<Context>) -> Result<(), Error> {
        let client = context.client.clone();
        let config = &self.spec.config;
        let ports = &config.ports;

        // Fetch info about the meta and data mounts
        let data_sources = self.get_capacities(context.clone()).await?;

        // Map them into the expected configuration format
        let data_sources = data_sources
            .into_iter()
            .enumerate()
            .map(|(index, capacity)| {
                format!(
                    r#"{{ path = "/mnt/disk{index}", capacity = "{}B" }}"#,
                    capacity.to_bytes_usize().unwrap(),
                )
            })
            .collect::<Vec<_>>();

        // Construct the config
        let garage_config = formatdoc! {r#"
                metadata_dir = "/mnt/meta"
                data_dir     = [ {data_sources} ]
                db_engine    = "lmdb"

                replication_mode = "{replication_mode}"

                # RPC info
                rpc_secret_file = "/secrets/rpc.key"
                rpc_bind_addr   = "[::]:{port_rpc}"

                [s3_api]
                s3_region = "{region}"
                api_bind_addr = "[::]:{port_s3}"

                [s3_web]
                bind_addr = "[::]:{port_web}"
                root_domain = ".web.garage.localhost"
                index = "index.html"

                [admin]
                api_bind_addr = "0.0.0.0:{port_admin}"
                admin_token_file = "/secrets/admin.key"
            "#,
            data_sources = data_sources.join(","),
            port_admin = ports.admin,
            port_rpc = ports.rpc,
            port_s3 = ports.s3_api,
            port_web = ports.s3_web,
            region = config.region,
            replication_mode = config.replication_mode,
        };

        // Make the ConfigMap for the config
        let owner = self.controller_owner_ref(&()).unwrap();
        let name = self.prefixed_name("config");
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalGarage(name.clone(), "missing namespace".to_string()))?;
        let cm = ConfigMap {
            metadata: meta! { owners: vec![owner], name: Some(name.clone()) },
            data: Some(BTreeMap::from([("garage.toml".into(), garage_config)])),

            binary_data: None,
            immutable: None,
        };

        // Apply the config
        let configs = Api::<ConfigMap>::namespaced(client.clone(), &namespace);
        let params = PatchParams::apply("garage-operator");
        let patch = Patch::Apply(cm);
        configs.patch(&name, &params, &patch).await?;

        Ok(())
    }

    /// Create the main deployment for running garage using the official docker container
    async fn create_deployment(&self, context: Arc<Context>) -> Result<(), Error> {
        let client = &context.client;

        // Extract needed info from the garage instance
        let name = self.name_any();
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalGarage(name.clone(), "missing namespace".into()))?;

        let storage = &self.spec.storage;
        let config = &self.spec.config;
        let ports = &config.ports;

        let service_ports = [
            ("s3-api", ports.s3_api),
            ("rpc", ports.rpc),
            ("s3-web", ports.s3_web),
            ("admin", ports.admin),
        ];

        // Generate metadata needed for managing the deployment through the operator
        let labels = labels! { instance: name.clone() };
        let owner = self.controller_owner_ref(&()).unwrap();

        // Create the deployment
        let deployment_data = Deployment {
            metadata: meta! {
                owners: vec![owner.clone()],
                name: Some(name.clone())
            },

            spec: Some(DeploymentSpec {
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    match_expressions: None,
                },
                template: PodTemplateSpec {
                    metadata: Some(meta! { owners: vec![owner], labels: Some(labels) }),
                    spec: Some(PodSpec {
                        // Use the official container from garage
                        containers: vec![Container {
                            image: Some(format!("dxflrs/garage:{}", context.garage_version)),
                            name: "garage".into(),

                            // Export the ports that we need
                            ports: Some(
                                service_ports
                                    .into_iter()
                                    .map(|(name, port)| ContainerPort {
                                        name: Some(name.into()),
                                        container_port: port as i32,
                                        ..Default::default()
                                    })
                                    .collect(),
                            ),

                            // Mount the needed secrets, config, and volumes
                            volume_mounts: Some(
                                [
                                    vec![
                                        VolumeMount {
                                            name: "config".into(),
                                            read_only: Some(true),
                                            mount_path: "/etc/garage.toml".into(),
                                            sub_path: Some("garage.toml".into()),
                                            ..Default::default()
                                        },
                                        VolumeMount {
                                            name: "admin-secret".into(),
                                            read_only: Some(true),
                                            mount_path: format!("/secrets/admin.key"),
                                            sub_path: Some("key".into()),
                                            ..Default::default()
                                        },
                                        VolumeMount {
                                            name: "rpc-secret".into(),
                                            read_only: Some(true),
                                            mount_path: format!("/secrets/rpc.key"),
                                            sub_path: Some("key".into()),
                                            ..Default::default()
                                        },
                                        VolumeMount {
                                            name: "meta-pvc".into(),
                                            mount_path: format!("/mnt/meta"),
                                            ..Default::default()
                                        },
                                    ],
                                    self.spec
                                        .storage
                                        .data
                                        .iter()
                                        .enumerate()
                                        .map(|(index, _)| VolumeMount {
                                            name: format!("data-pvc-{index}"),
                                            mount_path: format!("/mnt/data{index}"),
                                            ..Default::default()
                                        })
                                        .collect(),
                                ]
                                .concat(),
                            ),
                            ..Default::default()
                        }],

                        // Inform the container as to which volumes will be used
                        // and how they are mapped to existing resources
                        volumes: Some(
                            [
                                vec![
                                    Volume {
                                        name: "config".into(),
                                        config_map: Some(ConfigMapVolumeSource {
                                            name: Some(self.prefixed_name("config")),
                                            ..Default::default()
                                        }),
                                        ..Default::default()
                                    },
                                    Volume {
                                        name: "admin-secret".into(),
                                        secret: Some(SecretVolumeSource {
                                            secret_name: Some(
                                                self.spec
                                                    .secrets
                                                    .admin
                                                    .as_ref()
                                                    .and_then(|a| a.name.clone())
                                                    .unwrap_or(self.prefixed_name("admin.key")),
                                            ),
                                            default_mode: Some(0o600),
                                            ..Default::default()
                                        }),
                                        ..Default::default()
                                    },
                                    Volume {
                                        name: "rpc-secret".into(),
                                        secret: Some(SecretVolumeSource {
                                            secret_name: Some(
                                                self.spec
                                                    .secrets
                                                    .rpc
                                                    .as_ref()
                                                    .and_then(|a| a.name.clone())
                                                    .unwrap_or(self.prefixed_name("rpc.key")),
                                            ),
                                            default_mode: Some(0o600),
                                            ..Default::default()
                                        }),
                                        ..Default::default()
                                    },
                                    Volume {
                                        name: "meta-pvc".into(),
                                        persistent_volume_claim: Some(
                                            PersistentVolumeClaimVolumeSource {
                                                claim_name: storage.meta.clone(),
                                                read_only: None,
                                            },
                                        ),
                                        ..Default::default()
                                    },
                                ],
                                self.spec
                                    .storage
                                    .data
                                    .iter()
                                    .enumerate()
                                    .map(|(index, d)| Volume {
                                        name: format!("data-pvc-{index}"),
                                        persistent_volume_claim: Some(
                                            PersistentVolumeClaimVolumeSource {
                                                claim_name: d.clone(),
                                                read_only: None,
                                            },
                                        ),
                                        ..Default::default()
                                    })
                                    .collect(),
                            ]
                            .concat(),
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        // Apply the deployment
        let deployments = Api::<Deployment>::namespaced(client.clone(), &namespace);
        let params = PatchParams::apply("garage-operator");
        let patch = Patch::Apply(deployment_data);
        deployments.patch(&name, &params, &patch).await?;

        Ok(())
    }

    /// Optionally generates the needed secrets for this instance of a garage.
    ///
    /// Secrets can be also manually specified in the spec, which allows for the
    /// user to provide the secrets, if necessary.
    async fn create_secrets(&self, context: Arc<Context>) -> Result<(), Error> {
        let client = &context.client;

        // Extract needed info from the garage
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalGarage(self.name_any(), "missing namespace".into()))?;
        let secret_references = &self.spec.secrets;
        let owner = self.controller_owner_ref(&()).unwrap();

        // Get an API handle over all secrets in the target namespace
        let secrets_handle = Api::<Secret>::namespaced(client.clone(), &namespace);

        // Specify the secrets that we either need to generate, if not overridden by the config
        let needed_secrets = [
            (&secret_references.admin, self.prefixed_name("admin.key")),
            (&secret_references.rpc, self.prefixed_name("rpc.key")),
        ];

        // Generate the secrets
        for (reference, secret_id) in needed_secrets {
            // Skip creating the secret if there is a valid entry for it in the CRD or if
            // it was already generated
            if reference.is_some() || secrets_handle.get_opt(&secret_id).await?.is_some() {
                continue;
            }

            // Garage RPC requires 32 bytes of hex, so we'll just default to this for all secrets
            let secret_value = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

            // Generate the secret
            let secret = Secret {
                metadata: meta! { owners: vec![owner.clone()], name: Some(secret_id.clone()) },
                string_data: Some(BTreeMap::from([("key".into(), secret_value)])),

                ..Default::default()
            };

            secrets_handle
                .patch(
                    &secret_id,
                    &PatchParams::apply("garage-operator"),
                    &Patch::Apply(secret),
                )
                .await?;
        }

        Ok(())
    }

    /// Create the services exposed by the garage instance.
    async fn create_services(&self, context: Arc<Context>) -> Result<(), Error> {
        let client = context.client.clone();

        // Extract needed info from the garage
        let name = self.name_any();
        let service_name = self.prefixed_name("api");
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalGarage(name.clone(), "missing namespace".into()))?;
        let owner = self.controller_owner_ref(&()).unwrap();
        let ports = &self.spec.config.ports;
        let garage_services = [
            ("admin", ports.admin),
            ("rpc", ports.rpc),
            ("s3-api", ports.s3_api),
            ("s3-web", ports.s3_web),
        ];

        // Get an API handle to the services
        let services_handle = Api::<Service>::namespaced(client, &namespace);

        // Generate the service
        let service = Service {
            metadata: meta! {
                owners: vec![owner],
                name: Some(service_name.clone()),
                labels: Some(labels! { instance: name.clone() })
            },
            spec: Some(ServiceSpec {
                selector: Some(labels! { instance: name.clone() }),
                ports: Some(
                    garage_services
                        .into_iter()
                        .map(|(port_name, port)| ServicePort {
                            name: Some(port_name.to_string()),
                            port: port as i32,
                            protocol: Some("TCP".into()),
                            target_port: Some(IntOrString::Int(port as i32)),

                            ..Default::default()
                        })
                        .collect(),
                ),

                ..Default::default()
            }),
            status: None,
        };

        // Apply the service
        let patch = Patch::Apply(service);
        let params = PatchParams::apply("garage-operator");
        services_handle
            .patch(&service_name, &params, &patch)
            .await?;

        Ok(())
    }

    /// Return a list of capacities used by each of the specified data sources
    pub(crate) async fn get_capacities(
        &self,
        context: Arc<Context>,
    ) -> Result<Vec<ParsedQuantity>, Error> {
        let client = context.client.clone();

        let name = self.name_any();
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalGarage(name, "missing namespace".into()))?;
        let sources = &self.spec.storage.data;

        let api = Api::<PersistentVolumeClaim>::namespaced(client.clone(), &namespace);

        // Fetch the pvc info for each source
        let mut source_info = Vec::with_capacity(sources.len());
        for source in sources {
            info!(r#"Fetching info for source "{source}""#);
            let info = api
                .get_opt(&source)
                .await?
                .ok_or(Error::MissingDataSource(source.clone()))?;

            // TODO: Is this what we should do here?
            let capacity: ParsedQuantity = info
                .status
                .unwrap()
                .capacity
                .unwrap()
                .into_values()
                .map(|q| ParsedQuantity::try_from(q).unwrap())
                .fold(ParsedQuantity::default(), |acc, cur| acc + cur);
            info!(r#"Source "{source}" has capacity {capacity}"#);

            source_info.push(capacity);
        }

        Ok(source_info)
    }
}

impl Garage {
    /// Generate a name with the garage instance as a prefix
    pub fn prefixed_name(&self, rest: impl AsRef<str>) -> String {
        format!("{}-{}", self.name_any(), rest.as_ref())
    }
}
