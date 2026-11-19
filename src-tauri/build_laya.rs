use std::{env, path::PathBuf, process::Command};

pub fn embed() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        return;
    }
    for input in ["laya-worker/Cargo.toml", "laya-worker/Cargo.lock", "laya-worker/src", "src/laya_artifacts.rs", "src/laya_question.rs", "src/http_ingress.rs", "../scripts/laya/rust-bundle.json"] {
        println!("cargo:rerun-if-changed={input}");
    }
    let target = env::var("TARGET").expect("Cargo target");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let directory = output.ancestors().nth(4).expect("Cargo target directory").join("laya-worker");
    let mut cargo = Command::new(env::var_os("CARGO").expect("Cargo executable"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cargo.creation_flags(0x08000000);
    }
    let status = cargo.args(["build", "--locked", "--manifest-path", "laya-worker/Cargo.toml", "--profile", "worker", "--target", &target, "--target-dir"])
        .arg(&directory).args(["--jobs", "4"]).env_remove("CARGO_MAKEFLAGS").env_remove("MAKEFLAGS").env_remove("MFLAGS")
        .status().expect("start isolated Rust Laya build");
    assert!(status.success(), "Could not build the embedded Rust Laya worker");
    let binary = directory.join(target).join("worker/synaroute-laya-worker.exe");
    println!("cargo:rustc-env=SYNAROUTE_LAYA_WORKER={}", binary.display());
}
