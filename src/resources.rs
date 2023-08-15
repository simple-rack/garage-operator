use std::collections::{BTreeMap, HashMap, HashSet};

use futures::{future::try_join_all, try_join};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, PersistentVolumeClaim,
            PersistentVolumeClaimSpec, PersistentVolumeClaimVolumeSource, PodSpec, PodTemplateSpec,
            ResourceRequirements, Secret, SecretVolumeSource, Service, ServicePort, ServiceSpec,
            Volume, VolumeMount,
        },
    },
    apimachinery::pkg::{
        api::resource::Quantity, apis::meta::v1::LabelSelector, util::intstr::IntOrString,
    },
};
use kube::{
    api::{ListParams, Patch, PatchParams, PostParams},
    Api, Client, Resource, ResourceExt,
};
use serde_json::json;
use uuid::Uuid;

mod access_key;
mod bucket;
mod garage;

pub use access_key::*;
pub use bucket::*;
pub use garage::*;

use crate::{garage_admin::GarageAdmin, Error, Result, GARAGE_VERSION};

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

impl Garage {
    pub(crate) fn prefixed_name(&self, rest: &str) -> String {
        format!("{}-{rest}", self.name_any())
    }

    pub async fn create_admin<'a>(&'a self, client: Client) -> Result<GarageAdmin<'a>> {
        let token = {
            let default_name = self.prefixed_name("admin.key");
            let admin_token_name = self
                .spec
                .secrets
                .as_ref()
                .and_then(|s| s.admin.as_ref())
                .and_then(|a| a.secret_name.as_ref())
                .unwrap_or(&default_name);

            let secrets =
                Api::<Secret>::namespaced(client, &self.namespace().ok_or(Error::IllegalGarage)?);

            let secret = secrets
                .get(&admin_token_name)
                .await
                .map_err(Error::KubeError)?;
            let token = secret.data.ok_or(Error::IllegalGarage)?;
            let token = token.get("key").ok_or(Error::IllegalGarage)?;

            String::from_utf8(token.0.clone()).unwrap()
        };

        Ok(GarageAdmin::with_secret(&self, &token)?)
    }

    pub async fn handle_buckets(&self, client: Client, buckets: &[Bucket]) -> Result<()> {
        // First see which buckets we have currently configured
        let admin = self.create_admin(client.clone()).await?;
        let owner = self.controller_owner_ref(&()).unwrap();

        // Sets of the IDs for easy set operations
        let current_buckets = admin.get_buckets().await?;
        let current_buckets: HashSet<&String> = current_buckets.iter().collect();
        let managed_buckets: HashSet<&String> = buckets
            .iter()
            .filter_map(|b| b.annotations().get("bucket/id"))
            .collect();

        let bucket_map: HashMap<&String, &Bucket> = buckets
            .iter()
            .filter_map(|b| b.annotations().get("bucket/id").map(|id| (id, b)))
            .collect();

        // Helper function to consolidate access keys for buckets
        macro_rules! consolidate_keys {
            ($id:expr, $bucket:expr) => {
                async {
                    let ns = self.namespace().unwrap();

                    let access_keys =
                        Api::<AccessKey>::namespaced(client.clone(), &self.namespace().unwrap());
                    let owned_keys: Vec<_> = access_keys
                        .list(&ListParams::default())
                        .await
                        .map_err(Error::KubeError)?
                        .into_iter()
                        .filter(|ak| ak.spec.bucket_ref == format!("{ns}/{}", $bucket.name_any()))
                        .collect();
                    self.handle_access_keys(client.clone(), $id, $bucket, &owned_keys)
                        .await?;

                    Ok(())
                }
            };
        }

        // See which buckets need to be created
        // TODO: Should we do something else if we have a managed bucket with an ID that isn't in the
        // list of currently enabled buckets?
        let create_handlers = buckets
            .iter()
            .filter(|b| {
                // A bucket is marked for creation if it does not have an ID label or is currently
                // not enabled (by its ID) in the garage instance.
                let id = b.annotations().get("bucket/id");

                id.is_none() || !current_buckets.contains(id.unwrap())
            })
            .map(|b| async {
                let name = b.name_any();
                let ns = b.namespace().unwrap();

                let id = admin.create_bucket(name.clone()).await?;
                let buckets = Api::<Bucket>::namespaced(client.clone(), &ns);

                let new_label = Patch::Apply(json!({
                    "apiVersion": "deuxfleurs.fr/v0alpha",
                    "kind": "Bucket",
                    "metadata": {
                        "annotations": {
                            "bucket/id": id.clone(),
                        },
                        "ownerReferences": [owner],
                    },
                }));

                let ps = PatchParams::apply("garage-operator").force();
                let _o = buckets
                    .patch_metadata(&name, &ps, &new_label)
                    .await
                    .map_err(Error::KubeError)?;

                // Now update the info for the newly created bucket
                admin.update_bucket(&id, &b.spec).await?;
                consolidate_keys!(&id, b).await
            });

        // See which buckets must DIE
        let delete_handlers = current_buckets
            .difference(&managed_buckets)
            .map(|id| admin.delete_bucket(id));

        // Now configure them all
        // TODO: Can we be smart about this without making a bunch of network requests?
        let update_handlers = managed_buckets
            .intersection(&current_buckets)
            .map(|id| async {
                // TODO: Add explanation why this is ok to unwrap
                let bucket = bucket_map.get(id).unwrap();

                admin.update_bucket(id, &bucket.spec).await?;
                consolidate_keys!(id, bucket).await
            });

        // Actually handle all of the state management
        try_join!(
            try_join_all(create_handlers),
            try_join_all(delete_handlers),
            try_join_all(update_handlers)
        )?;

        Ok(())
    }

    pub async fn handle_access_keys(
        &self,
        client: Client,
        owning_bucket_id: &str,
        owning_bucket: &Bucket,
        access_keys: &[AccessKey],
    ) -> Result<()> {
        // First see which buckets we have currently configured
        let admin = self.create_admin(client.clone()).await?;
        let owner = owning_bucket.controller_owner_ref(&()).unwrap();

        // Sets of the IDs for easy set operations
        let current_access_keys = admin.get_access_keys().await?;
        let current_access_keys: HashSet<&String> = current_access_keys.iter().collect();
        let managed_access_keys: HashSet<&String> = access_keys
            .iter()
            .filter_map(|b| b.annotations().get("access-key/id"))
            .collect();

        let access_key_map: HashMap<&String, &AccessKey> = access_keys
            .iter()
            .filter_map(|b| b.annotations().get("access-key/id").map(|id| (id, b)))
            .collect();

        // See which buckets need to be created
        // TODO: Should we do something else if we have a managed bucket with an ID that isn't in the
        // list of currently enabled buckets?
        let create_handlers = access_keys
            .iter()
            .filter(|ak| {
                // A access key is marked for creation if it does not have an ID label or is currently
                // not enabled (by its ID) in the garage instance.
                let id = ak.annotations().get("access-key/id");

                id.is_none() || !current_access_keys.contains(id.unwrap())
            })
            .map(|ak| async {
                let name = ak.name_any();
                let ns = ak.namespace().unwrap();

                let key = admin.create_access_key(name.clone()).await?;
                let access_keys = Api::<AccessKey>::namespaced(client.clone(), &ns);

                let new_label = Patch::Apply(json!({
                    "apiVersion": "deuxfleurs.fr/v0alpha",
                    "kind": "AccessKey",
                    "metadata": {
                        "annotations": {
                            "access-key/id": key.access_key_id.clone(),
                        },
                        "ownerReferences": [owner],
                    },
                }));

                // Actually create the k8s secret
                {
                    let key_owner = ak.controller_owner_ref(&()).unwrap();
                    let key_secret = Secret {
                        immutable: Some(true),
                        metadata: meta! { owners: vec![key_owner.clone()],
                            name: ak.spec.secret_ref.clone().or(
                                Some(
                                    format!(
                                        "{}.{}.{}",
                                        ak.name_any(),
                                        owning_bucket.name_any(),
                                        self.name_any()
                                    )
                                )
                            )
                        },
                        string_data: Some(BTreeMap::from([
                            ("access-key".into(), key.access_key_id.clone().unwrap()),
                            ("secret-key".into(), key.secret_access_key.clone().unwrap()),
                        ])),

                        ..Default::default()
                    };

                    let secrets =
                        Api::<Secret>::namespaced(client.clone(), &ak.namespace().unwrap());
                    let _o = secrets
                        .create(
                            &PostParams {
                                field_manager: Some("garage-operator".into()),

                                ..Default::default()
                            },
                            &key_secret,
                        )
                        .await
                        .map_err(Error::KubeError)?;
                }

                // Patch the secret key to have its info
                let ps = PatchParams::apply("garage-operator").force();
                let _o = access_keys
                    .patch_metadata(&name, &ps, &new_label)
                    .await
                    .map_err(Error::KubeError)?;

                // Now update the info for the newly created bucket
                admin
                    .update_access_key(&owning_bucket_id, &key.access_key_id.unwrap(), &ak.spec)
                    .await
            });

        // See which access keys must DIE
        let delete_handlers = current_access_keys
            .difference(&managed_access_keys)
            .map(|id| admin.delete_access_key(id));

        // Now configure them all
        // TODO: Can we be smart about this without making a bunch of network requests?
        let update_handlers =
            managed_access_keys
                .intersection(&current_access_keys)
                .map(|id| async {
                    // TODO: Add explanation why this is ok to unwrap
                    let ak = access_key_map.get(id).unwrap();

                    let bucket_id = owning_bucket.annotations().get("bucket/id").unwrap();
                    admin.update_access_key(&bucket_id, id, &ak.spec).await
                });

        // Actually handle all of the state management
        try_join!(
            try_join_all(create_handlers),
            try_join_all(delete_handlers),
            try_join_all(update_handlers)
        )?;

        Ok(())
    }

    pub async fn deploy_resources(&self, client: Client) -> Result<()> {
        try_join!(
            self.create_config(client.clone()),
            self.create_secrets(client.clone()),
            self.create_services(client.clone()),
            self.create_volumes(client.clone()),
        )?;

        // Now deploy
        self.create_deployment(client.clone()).await?;

        Ok(())
    }

    async fn create_config(&self, client: Client) -> Result<()> {
        let config = self.spec.config.clone().unwrap_or_default();
        let ports = config.ports;

        let garage_config = format!(
            r#"
            metadata_dir = "/mnt/meta"
            data_dir     = "/mnt/data"
            db_engine    = "lmdb"

            replication_mode = "{}"

            # RPC info
            rpc_secret_file = "/secrets/rpc.key"
            rpc_bind_addr   = "[::]:{}"

            [s3_api]
            s3_region = "{}"
            api_bind_addr = "[::]:{}"

            [s3_web]
            bind_addr = "[::]:{}"
            root_domain = ".web.garage.localhost"
            index = "index.html"

            [admin]
            api_bind_addr = "0.0.0.0:{}"
            admin_token_file = "/secrets/admin.key"
        "#,
            config.replication_mode,
            ports.rpc,
            config.region,
            ports.s3_api,
            ports.s3_web,
            ports.admin,
        );

        let owner = self.controller_owner_ref(&()).unwrap();
        let name = self.prefixed_name("config");
        let namespace = self.namespace().ok_or(Error::IllegalGarage)?;
        let cm = ConfigMap {
            metadata: meta! { owners: vec![owner], name: Some(name.clone()) },
            data: Some(BTreeMap::from([("garage.toml".into(), garage_config)])),

            binary_data: None,
            immutable: None,
        };

        let configs = Api::<ConfigMap>::namespaced(client.clone(), &namespace);
        let params = PatchParams::apply("garage-operator");
        let patch = Patch::Apply(cm);
        configs
            .patch(&name, &params, &patch)
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    async fn create_deployment(&self, client: Client) -> Result<()> {
        let name = self.name_any();
        let namespace = self.namespace().ok_or(Error::IllegalGarage)?;
        let labels = labels! { instance: name.clone() };
        let owner = self.controller_owner_ref(&()).unwrap();

        let storage = self.spec.storage.clone().unwrap_or_default();
        let config = self.spec.config.clone().unwrap_or_default();
        let ports = config.ports;

        let service_ports = [
            ("s3-api", ports.s3_api),
            ("rpc", ports.rpc),
            ("s3-web", ports.s3_web),
            ("admin", ports.admin),
        ];

        let deployment_data = Deployment {
            metadata: meta! { owners: vec![owner.clone()],
                name: Some(name.clone())
            },

            spec: Some(DeploymentSpec {
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    match_expressions: None,
                },
                template: PodTemplateSpec {
                    metadata: Some(meta! { owners: vec![owner], labels: Some(labels.clone()) }),
                    spec: Some(PodSpec {
                        containers: vec![Container {
                            image: Some(format!("dxflrs/garage:v{GARAGE_VERSION}")),
                            name: "garage".into(),
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
                            volume_mounts: Some(vec![
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
                                VolumeMount {
                                    name: "data-pvc".into(),
                                    mount_path: format!("/mnt/data"),
                                    ..Default::default()
                                },
                            ]),

                            ..Default::default()
                        }],
                        volumes: Some(vec![
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
                                            .clone()
                                            .and_then(|s| s.admin)
                                            .and_then(|a| a.secret_name)
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
                                            .clone()
                                            .and_then(|s| s.rpc)
                                            .and_then(|a| a.secret_name)
                                            .unwrap_or(self.prefixed_name("rpc.key")),
                                    ),
                                    default_mode: Some(0o600),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: "meta-pvc".into(),
                                persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                                    claim_name: storage
                                        .meta
                                        .existing_claim
                                        .unwrap_or(self.prefixed_name("meta")),
                                    read_only: Some(false),
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: "data-pvc".into(),
                                persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                                    claim_name: storage
                                        .data
                                        .existing_claim
                                        .unwrap_or(self.prefixed_name("data")),
                                    read_only: Some(false),
                                }),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        let deployments = Api::<Deployment>::namespaced(client.clone(), &namespace);
        let params = PatchParams::apply("garage-operator");
        let patch = Patch::Apply(deployment_data);
        deployments
            .patch(&name, &params, &patch)
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    /// Optionally generates the needed secrets for this instance of a garage.
    ///
    /// Secrets can be also manually specified in the spec, which allows for the
    /// user to manually specify the secrets, if necessary.
    async fn create_secrets(&self, client: Client) -> Result<()> {
        let namespace = self.namespace().ok_or(Error::IllegalGarage)?;
        let secret_references = self.spec.secrets.clone().unwrap_or_default();
        let secrets = Api::<Secret>::namespaced(client.clone(), &namespace);
        let owner = self.controller_owner_ref(&()).unwrap();

        let needed_secrets = [
            (secret_references.admin, self.prefixed_name("admin.key")),
            (secret_references.rpc, self.prefixed_name("rpc.key")),
        ];
        for (reference, secret_id) in needed_secrets {
            // Skip creating the secret if there is a valid entry for it in the CRD
            if reference.is_some() {
                continue;
            }

            // Garage RPC requires 32 bytes of hex, so we'll just default to this for all secrets
            let secret_value = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

            let secret = Secret {
                metadata: meta! { owners: vec![owner.clone()], name: Some(secret_id) },
                string_data: Some(BTreeMap::from([("key".into(), secret_value)])),

                ..Default::default()
            };

            secrets
                .create(
                    &PostParams {
                        field_manager: Some("garage-operator".into()),

                        ..Default::default()
                    },
                    &secret,
                )
                .await
                .map_err(Error::KubeError)?;
        }

        Ok(())
    }

    /// Create the services exposed by the garage instance.
    async fn create_services(&self, client: Client) -> Result<()> {
        let ports = self.spec.config.clone().unwrap_or_default().ports;
        let garage_services = [
            ("admin", vec![("admin", ports.admin)]),
            (
                "s3",
                vec![("s3-api", ports.s3_api), ("s3-web", ports.s3_web)],
            ),
            ("rpc", vec![("rpc", ports.rpc)]),
        ];

        let services = Api::<Service>::namespaced(
            client.clone(),
            &self.namespace().ok_or(Error::IllegalGarage)?,
        );
        for (service_name, ports) in garage_services {
            let owner = self.controller_owner_ref(&()).unwrap();
            let name = self.prefixed_name(service_name);

            // info!("patching service: {name}");
            let params = PatchParams::apply("garage-operator");
            let patch = Patch::Apply(Service {
                metadata: meta! {
                    owners: vec![owner],
                    name: Some(name.clone().into()),
                    labels: Some(labels! { instance: self.name_any() })
                },
                spec: Some(ServiceSpec {
                    selector: Some(labels! { instance: self.name_any() }),
                    ports: Some(
                        ports
                            .into_iter()
                            .map(|(name, port)| ServicePort {
                                name: Some(name.into()),
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
            });

            services
                .patch(&name, &params, &patch)
                .await
                .map_err(|e| Error::KubeError(e))?;
        }

        Ok(())
    }

    async fn create_volumes(&self, client: Client) -> Result<()> {
        let config = self.spec.storage.clone().unwrap_or_default();
        let claims = [
            (self.prefixed_name("meta"), "100Mi", config.meta),
            (self.prefixed_name("data"), "500Mi", config.data),
        ];

        let pvcs = Api::<PersistentVolumeClaim>::namespaced(
            client.clone(),
            &self.namespace().ok_or(Error::IllegalGarage)?,
        );
        let params = PatchParams::apply("garage-operator");
        for (name, default_size, conf) in claims {
            // Only do this if we haven't specified a volume claim
            if conf.existing_claim.is_some() {
                continue;
            }

            let owner = self.controller_owner_ref(&()).unwrap();
            let claim_size = conf.size.unwrap_or(default_size.into());
            let claim = PersistentVolumeClaim {
                metadata: meta! {
                    owners: vec![owner],
                    name: Some(name.clone())
                },
                spec: Some(PersistentVolumeClaimSpec {
                    access_modes: Some(vec!["ReadWriteOnce".into()]),
                    storage_class_name: conf.storage_class,
                    resources: Some(ResourceRequirements {
                        requests: Some(BTreeMap::from([(
                            "storage".into(),
                            Quantity(claim_size.clone()),
                        )])),
                        limits: None,
                    }),

                    ..Default::default()
                }),
                status: None,
            };

            let patch = Patch::Apply(claim);
            pvcs.patch(&name, &params, &patch)
                .await
                .map_err(Error::KubeError)?;
        }

        Ok(())
    }
}
