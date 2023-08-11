//! Auto generate a garage admin client in rust based on the OpenAPI spec.

fn main() {
    let admin_api = "./spec/garage-admin-v0.yml";
    println!("cargo:rerun-if-changed={admin_api}");

    let file = std::fs::File::open(admin_api).expect("could not open Garage admin OpenAPI spec");
    let spec = serde_yaml::from_reader(file).expect("could not read Garage admin OpenAPI spec");

    let mut generator = progenitor::Generator::default();

    let tokens = generator
        .generate_tokens(&spec)
        .expect("could not generate tokens from spec");
    let ast = syn::parse2(tokens).expect("internal error for progenitor dependency");
    let content = prettyplease::unparse(&ast);

    let mut generated = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).to_path_buf();
    generated.push("garage-admin-client.rs");

    std::fs::write(generated, content).expect("could not write out generated client code");
}
