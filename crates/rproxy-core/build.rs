use std::{
    env,
    ffi::OsString,
    fs,
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
    copy_wintun_runtime(&vendor_dir);
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

    let signature = hev_build_signature(&make, &cflags);
    clean_vendored_hev_if_needed(vendor_dir, &signature);

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

    write_hev_build_signature(vendor_dir, &signature);
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

fn hev_build_signature(make: &str, cflags: &str) -> String {
    format!(
        "target={}\nenv={}\nmake={make}\ncc={}\nar={}\ncflags={cflags}\nsource={}\n",
        env::var("TARGET").unwrap_or_default(),
        env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default(),
        env::var("RPROXY_HEV_CC").unwrap_or_default(),
        env::var("RPROXY_HEV_AR").unwrap_or_default(),
        hev_source_signature(),
    )
}

fn hev_source_signature() -> String {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    ["src/hev-socks5-tunnel.c", "src/hev-tunnel-windows.c"]
        .into_iter()
        .map(|source| {
            let path = manifest_dir.join(HEV_VENDOR_DIR).join(source);
            let modified = fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos().to_string())
                .unwrap_or_else(|| "unknown".into());
            format!("{source}:{modified}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn clean_vendored_hev_if_needed(vendor_dir: &Path, signature: &str) {
    let stamp = vendor_dir.join("build/.rproxy-build-signature");
    if fs::read_to_string(&stamp)
        .map(|current| current == signature)
        .unwrap_or(false)
    {
        return;
    }

    for dir in [
        vendor_dir.join("build"),
        vendor_dir.join("bin"),
        vendor_dir.join("third-part/yaml/build"),
        vendor_dir.join("third-part/yaml/bin"),
        vendor_dir.join("third-part/lwip/build"),
        vendor_dir.join("third-part/lwip/bin"),
        vendor_dir.join("third-part/hev-task-system/build"),
        vendor_dir.join("third-part/hev-task-system/bin"),
    ] {
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap_or_else(|error| {
                panic!(
                    "failed to remove stale vendored build dir {}: {error}",
                    dir.display()
                )
            });
        }
    }
}

fn write_hev_build_signature(vendor_dir: &Path, signature: &str) {
    let stamp = vendor_dir.join("build/.rproxy-build-signature");
    if let Some(parent) = stamp.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!(
                "failed to create vendored build stamp dir {}: {error}",
                parent.display()
            )
        });
    }
    fs::write(&stamp, signature).unwrap_or_else(|error| {
        panic!(
            "failed to write vendored build stamp {}: {error}",
            stamp.display()
        )
    });
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

fn copy_wintun_runtime(vendor_dir: &Path) {
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let source = vendor_dir.join("third-part/wintun/bin/wintun.dll");
    println!("cargo:rerun-if-changed={}", source.display());
    if !source.exists() {
        println!(
            "cargo:warning=vendored wintun.dll was not found at {}; Tun mode will need wintun.dll beside the executable",
            source.display()
        );
        return;
    }

    let Ok(out_dir) = env::var("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let target = profile_dir.join("wintun.dll");
    fs::copy(&source, &target).unwrap_or_else(|error| {
        panic!(
            "failed to copy {} to {}: {error}",
            source.display(),
            target.display()
        )
    });
}
