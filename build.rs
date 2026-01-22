use std::{
    fs,
    io::Result,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=FORCE_MKFS");

    // build user programs
    let (uprogs_src_path, uprogs) = build_uprogs(&out_dir);

    // build mkfs
    let mkfs_path = build_mkfs(&out_dir);

    // build fs.img
    let fs_img = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("target")
        .join("fs.img");
    println!("cargo:rerun-if-changed={}", mkfs_path.display());
    println!("cargo:rerun-if-changed={}", uprogs_src_path.display());
    let readme = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    assert!(readme.exists(), "README.md not found");
    let force_mkfs = std::env::var("FORCE_MKFS").ok().as_deref() == Some("1");
    if !force_mkfs && fs_img.exists() {
        println!("cargo:warning=mkfs: keeping existing fs.img (set FORCE_MKFS=1 to rebuild)");
    } else {
        let mut mkfs_cmd = Command::new(&mkfs_path);
        mkfs_cmd.current_dir(&out_dir);
        mkfs_cmd.arg(fs_img).arg(&readme).args(uprogs);
        let status = mkfs_cmd.status().expect("mkfs fs.img failed to run");
        assert!(status.success(), "mkfs fs.img failed: {status}");
    }

    // linker script for kernel
    println!("cargo:rustc-link-arg-bin=web-os=--script=crates/kernel/kernel.ld");
}

fn build_uprogs(out_dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.arg("install").arg("uprogs");
    let local_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates")
        .join("user");
    if local_path.exists() {
        // local build
        cmd.arg("--path").arg(&local_path);
        println!("cargo:rerun-if-changed={}", local_path.display());
    }
    cmd.arg("--root").arg(out_dir);
    cmd.arg("-vv");
    let uprogs_target = out_dir.join("uprogs-target");
    cmd.env("CARGO_TARGET_DIR", &uprogs_target);
    cmd.env("ROOT_OUT_DIR", out_dir.to_str().unwrap()); // for libs and etc config
    cmd.env_remove("RUSTFLAGS");
    cmd.env_remove("CARGO_ENCODED_RUSTFLAGS");
    cmd.env_remove("RUSTC_WORKSPACE_WRAPPER");
    let status = cmd
        .status()
        .expect("failed to run cargo install for uprogs");
    if status.success() {
        let mut ufiles: Vec<PathBuf> = Vec::new();
        let mut collet_files = |dir: &Path, prefix: Option<&str>| {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.filter_map(Result::ok) {
                    let path = entry.path();
                    if path.is_file() {
                        let should_push = match (prefix, path.file_name().and_then(|s| s.to_str()))
                        {
                            (Some(prefix), Some(name)) if name.starts_with(prefix) => true,
                            (None, Some(_)) => true,
                            _ => false,
                        };
                        if should_push {
                            ufiles.push(path);
                        }
                    }
                }
            }
        };
        let dirs = ["bin", "etc", "lib"];
        for dir_ent in dirs {
            let path = out_dir.join(dir_ent);
            collet_files(&path, Some("_"));
        }
        (local_path, ufiles)
    } else {
        panic!("failed to build user programs");
    }
}

fn build_mkfs(out_dir: &Path) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.arg("install").arg("mkfs");
    let host = std::env::var("HOST").unwrap();
    cmd.arg("--target").arg(&host);
    let local_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates")
        .join("mkfs");
    if local_path.exists() {
        // local build
        cmd.arg("--path").arg(&local_path);
        println!("cargo:rerun-if-changed={}", local_path.display());
    }
    cmd.arg("--root").arg(out_dir);
    cmd.arg("-vv");
    let mkfs_target = out_dir.join("mkfs-target");
    cmd.env("CARGO_TARGET_DIR", &mkfs_target);
    cmd.env_remove("RUSTFLAGS");
    cmd.env_remove("CARGO_ENCODED_RUSTFLAGS");
    cmd.env_remove("RUSTC_WORKSPACE_WRAPPER");
    cmd.env_remove("CARGO_BUILD_TARGET");
    let status = cmd.status().expect("failed to run cargo install for mkfs");
    if status.success() {
        let mut path = out_dir.join("bin").join("mkfs");
        path.set_extension(std::env::consts::EXE_EXTENSION);
        assert!(path.exists(), "mkfs does not exist after building");
        path
    } else {
        panic!("failed to build mkfs");
    }
}
