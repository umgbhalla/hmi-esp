use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../.env.local");
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }

    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("../../.env.local");
    if let Ok(contents) = fs::read_to_string(path) {
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "WIFI_SSID" => println!("cargo:rustc-env=HMI_WIFI_SSID={}", value.trim()),
                "WIFI_PASSWORD" => {
                    println!("cargo:rustc-env=HMI_WIFI_PASSWORD={}", value.trim())
                }
                _ => {}
            }
        }
    }
}
