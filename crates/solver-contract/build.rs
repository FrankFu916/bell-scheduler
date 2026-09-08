#![forbid(unsafe_code)]

use std::{env, path::PathBuf};

fn main() {
    let proto = "proto/scheduler/v1/solver.proto";
    println!("cargo:rerun-if-changed={proto}");
    println!("cargo:rerun-if-changed=proto");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo always sets OUT_DIR"));
    let mut config = prost_build::Config::new();
    config.file_descriptor_set_path(out_dir.join("scheduler_v1_descriptor.bin"));
    config
        .compile_protos(&[proto], &["proto"])
        .expect("scheduler protocol protobuf must compile");
}
