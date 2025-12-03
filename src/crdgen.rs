use kube::CustomResourceExt;
use wadm_operator_types::v1alpha1::WasmCloudHostConfig;

fn main() {
    print!(
        "{}",
        serde_yaml::to_string(&WasmCloudHostConfig::crd()).unwrap()
    )
}
