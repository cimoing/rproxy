use std::{
    env, fs,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

const HEV_VENDOR_DIR: &str = "vendor/hev-socks5-tunnel";

fn main() {
    println!("cargo:rerun-if-env-changed=RPROXY_HEV_MAKE");
    println!("cargo:rerun-if-env-changed=RPROXY_HEV_CC");
    println!("cargo:rerun-if-env-changed=RPROXY_HEV_AR");
    println!("cargo:rerun-if-env-changed=RPROXY_HEV_CFLAGS");
    println!("cargo:rerun-if-changed={HEV_VENDOR_DIR}/Makefile");
    println!("cargo:rerun-if-changed={HEV_VENDOR_DIR}/build.mk");
    println!("cargo:rerun-if-changed={HEV_VENDOR_DIR}/src");
    println!("cargo:rerun-if-changed={HEV_VENDOR_DIR}/third-part");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let vendor_dir = manifest_dir.join(HEV_VENDOR_DIR);

    build_vendored_hev(&vendor_dir);
    link_vendored_hev(&vendor_dir);
}

fn build_vendored_hev(vendor_dir: &Path) {
    let make = env::var("RPROXY_HEV_MAKE").unwrap_or_else(|_| "make".into());
    let mut command = Command::new(&make);
    command
        .current_dir(vendor_dir)
        .arg("static")
        .arg("REV_ID=vendored");

    if env::var("CARGO_CFG_WINDOWS").is_ok() {
        command
            .arg("CONFIG_STACK_BACKEND=STACK_HEAP")
            .arg("ENABLE_STACK_OVERFLOW_DETECTION=0");
    }

    prepend_tool_dirs_to_path(&mut command, [&make, "RPROXY_HEV_CC", "RPROXY_HEV_AR"]);

    if let Ok(cc) = env::var("RPROXY_HEV_CC") {
        command.arg(format!("CC={cc}"));
    }
    if let Ok(ar) = env::var("RPROXY_HEV_AR") {
        command.arg(format!("AR={ar}"));
    }
    let mut cflags = default_hev_cflags();
    if let Ok(extra_cflags) = env::var("RPROXY_HEV_CFLAGS") {
        if !cflags.is_empty() {
            cflags.push(' ');
        }
        cflags.push_str(&extra_cflags);
    }
    if !cflags.is_empty() {
        command.arg(format!("CFLAGS={cflags}"));
    }

    let Ok(status) = command.status() else {
        println!(
            "cargo:warning=failed to start {make} for vendored hev-socks5-tunnel. \
             cargo check can continue, but cargo build/test needs make and a C toolchain; \
             set RPROXY_HEV_MAKE/RPROXY_HEV_CC/RPROXY_HEV_AR if needed."
        );
        return;
    };
    if !status.success() {
        panic!("vendored hev-socks5-tunnel static build failed with status {status}");
    }

    mirror_msvc_lib_names(vendor_dir);
}

fn default_hev_cflags() -> String {
    if env::var("CARGO_CFG_WINDOWS").is_ok() {
        "-D__MSYS__ -Wno-error=incompatible-pointer-types".into()
    } else {
        String::new()
    }
}

fn prepend_tool_dirs_to_path(command: &mut Command, tools: [&str; 3]) {
    let mut paths = Vec::new();

    add_parent_dir(&mut paths, Path::new(tools[0]));
    for env_name in tools.into_iter().skip(1) {
        if let Ok(tool) = env::var(env_name) {
            add_parent_dir(&mut paths, Path::new(&tool));
        }
    }

    if paths.is_empty() {
        return;
    }

    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }

    let joined = env::join_paths(paths).unwrap_or_else(|_| OsString::new());
    command.env("PATH", joined);
}

fn add_parent_dir(paths: &mut Vec<PathBuf>, tool: &Path) {
    if !tool.is_absolute() {
        return;
    }
    if let Some(parent) = tool.parent() {
        paths.push(parent.to_path_buf());
    }
}

fn link_vendored_hev(vendor_dir: &Path) {
    println!(
        "cargo:rustc-link-search=native={}",
        vendor_dir.join("bin").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        vendor_dir.join("third-part/yaml/bin").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        vendor_dir.join("third-part/lwip/bin").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        vendor_dir.join("third-part/hev-task-system/bin").display()
    );

    println!("cargo:rustc-link-lib=static=hev-socks5-tunnel");
    println!("cargo:rustc-link-lib=static=yaml");
    println!("cargo:rustc-link-lib=static=lwip");
    println!("cargo:rustc-link-lib=static=hev-task-system");

    if cfg!(windows) {
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=Iphlpapi");
    } else {
        println!("cargo:rustc-link-lib=pthread");
    }
}

fn mirror_msvc_lib_names(vendor_dir: &Path) {
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    for (source, target) in [
        ("bin/libhev-socks5-tunnel.a", "bin/hev-socks5-tunnel.lib"),
        (
            "third-part/yaml/bin/libyaml.a",
            "third-part/yaml/bin/yaml.lib",
        ),
        (
            "third-part/lwip/bin/liblwip.a",
            "third-part/lwip/bin/lwip.lib",
        ),
        (
            "third-part/hev-task-system/bin/libhev-task-system.a",
            "third-part/hev-task-system/bin/hev-task-system.lib",
        ),
    ] {
        let source = vendor_dir.join(source);
        let target = vendor_dir.join(target);
        if source.exists() {
            fs::copy(&source, &target).unwrap_or_else(|error| {
                panic!(
                    "failed to mirror {} to {} for MSVC link: {error}",
                    source.display(),
                    target.display()
                )
            });
        }
    }
}
