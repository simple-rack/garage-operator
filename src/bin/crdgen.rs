use kube::CustomResourceExt;

fn main() {
    let resources = [
        garage_operator::resources::AccessKey::crd(),
        garage_operator::resources::Garage::crd(),
        garage_operator::resources::Bucket::crd(),
    ];

    for resource in resources {
        println!("---");
        print!("{}", serde_yaml::to_string(&resource).unwrap());
    }
}
