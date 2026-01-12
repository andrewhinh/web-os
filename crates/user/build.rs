use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use kernel::syscall::*;

fn main() {
    let root_out_dir = std::env::var("ROOT_OUT_DIR").ok().map(PathBuf::from);

    if let Some(root_out_dir) = root_out_dir.as_ref() {
        // copy src/etc/_*, src/lib/_* to root_out_dir/*
        let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let src_dir = manifest_dir.join("src").join("etc");
        let dst_dir = root_out_dir.join("etc");
        copy_files(&src_dir, &dst_dir, Some("_")).expect("failed to copy user etc");

        let src_dir = manifest_dir.join("src").join("lib");
        let dst_dir = root_out_dir.join("lib");
        copy_files(&src_dir, &dst_dir, Some("_")).expect("failed to copy user lib");
    }

    // build syscall interface file usys.rs
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut usys_rs =
        File::create(out_dir.join("usys.rs")).expect("couldn't create OUT_DIR/usys.rs");
    usys_rs
        .write_all("// Created by build.rs\n\n".as_bytes())
        .expect("OUT_DIR/usys.rs: write error");
    for syscall_id in SysCalls::into_enum_iter().skip(1) {
        usys_rs
            .write_all(syscall_id.gen_usys().as_bytes())
            .expect("usys write error");
    }

    // set linker script
    let local_path = Path::new(env!("CARGO_MANIFEST_DIR"));
    println!(
        "cargo:rustc-link-arg=-T{}",
        local_path.join("user.ld").display()
    );
}

fn copy_files(src_dir: &Path, dst_dir: &Path, prefix: Option<&str>) -> io::Result<()> {
    if !src_dir.exists() {
        return Ok(());
    }
    if !dst_dir.exists() {
        fs::create_dir_all(dst_dir)?;
    }
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dst_path = dst_dir.join(entry.file_name());
        if entry_path.is_dir() {
            copy_files(&entry_path, &dst_path, prefix)?;
        } else {
            let should_copy = match (prefix, entry_path.file_name().and_then(|s| s.to_str())) {
                (Some(prefix), Some(name)) if name.starts_with(prefix) => true,
                (None, Some(_)) => true,
                _ => false,
            };
            if should_copy {
                fs::copy(&entry_path, &dst_path)?;
            }
        }
    }
    Ok(())
}
