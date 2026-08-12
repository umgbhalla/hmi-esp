fn main() {
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    println!("cargo:rerun-if-changed=bindings.h");
    println!("cargo:rerun-if-changed=components/hmi_touch349");
    println!("cargo:rerun-if-env-changed=HMI_WIFI_SSID");
    println!("cargo:rerun-if-env-changed=HMI_WIFI_PASSWORD");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }
}
