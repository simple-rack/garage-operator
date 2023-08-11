# Garage Operator

A rust kubernetes operator for [Garage](https://garagehq.deuxfleurs.fr/) using [kube](https://github.com/kube-rs/kube/), with observability instrumentation.

This project is based off of the [kubernetes reference controller](https://github.com/kube-rs/controller-rs) maintained by
the kube-rs project.

## Requirements
- A Kubernetes cluster / k3d instance
- The [CRD](yaml/crd.yaml)
- Opentelemetry collector (**optional**)

### Cluster
As an example; get `k3d` then:

```sh
k3d cluster create --registry-create --servers 1 --agents 1 main
k3d kubeconfig get --all > ~/.kube/k3d
export KUBECONFIG="$HOME/.kube/k3d"
```

A default `k3d` setup is fastest for local dev due to its local registry.

### CRD
Apply the CRD from [cached file](yaml/crd.yaml), or pipe it from `crdgen` (best if changing it):

```sh
cargo run --bin crdgen | kubectl apply -f -
```

### Opentelemetry
#### WARNING: Currently untested.
Setup an opentelemetry collector in your cluster. [Tempo](https://github.com/grafana/helm-charts/tree/main/charts/tempo) / [opentelemetry-operator](https://github.com/open-telemetry/opentelemetry-helm-charts/tree/main/charts/opentelemetry-operator) / [grafana agent](https://github.com/grafana/helm-charts/tree/main/charts/agent-operator) should all work out of the box. If your collector does not support grpc otlp you need to change the exporter in [`main.rs`](./src/main.rs).

If you don't have a collector, you can build locally without the `telemetry` feature (`tilt up telemetry`), or pull images [without the `otel` tag](https://hub.docker.com/r/clux/controller/tags/).

## Running

### Locally

```sh
cargo run --bin operator
```

or, with optional telemetry (change as per requirements):

```sh
OPENTELEMETRY_ENDPOINT_URL=https://0.0.0.0:55680 RUST_LOG=info,kube=trace,controller=debug cargo run --features=telemetry
```
