use flate2::write::GzEncoder;
use flate2::Compression;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_rc_exe() -> Option<PathBuf> {
    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if kits.exists() {
        if let Ok(entries) = fs::read_dir(&kits) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            versions.sort();
            versions.reverse();
            for v in versions {
                let candidate = v.join("x64").join("rc.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let profile = env::var("PROFILE").unwrap_or_else(|_| "release".into());
    let target_root = env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root.join("target"));
    let primary = target_root.join(&profile).join("puwbar.exe");
    let fallback_release = target_root.join("release").join("puwbar.exe");
    let fallback_debug = target_root.join("debug").join("puwbar.exe");

    let payload_src = [primary, fallback_release, fallback_debug]
        .into_iter()
        .find(|p| p.exists());

    let out_gz = out_dir.join("puwbar_payload.gz");
    if let Some(src) = payload_src {
        println!("cargo:rerun-if-changed={}", src.display());
        let bytes = fs::read(&src).expect("failed reading puwbar.exe payload");
        let file = File::create(&out_gz).expect("failed creating puwbar_payload.gz");
        let mut enc = GzEncoder::new(file, Compression::best());
        enc.write_all(&bytes).expect("failed compressing payload");
        enc.finish().expect("failed finishing gzip payload");
    } else {
        println!(
            "cargo:warning=puwbar.exe not found yet; embedding empty payload (build puwbar --release first)"
        );
        let file = File::create(&out_gz).expect("failed creating empty puwbar_payload.gz");
        let enc = GzEncoder::new(file, Compression::fast());
        enc.finish().expect("failed finishing empty gzip payload");
    }

    println!("cargo:rerun-if-changed=puwbar-setup.rc");
    println!("cargo:rerun-if-changed=puwbar-setup.manifest");
    println!("cargo:rerun-if-changed=../../assets/puwbar.ico");

    let rc_file = manifest_dir.join("puwbar-setup.rc");
    let res_file = out_dir.join("puwbar-setup.res");
    if rc_file.exists() {
        if let Some(rc_exe) = find_rc_exe() {
            let status = Command::new(&rc_exe)
                .args([
                    "/nologo",
                    "/fo",
                    &res_file.to_string_lossy(),
                    &rc_file.to_string_lossy(),
                ])
                .status();
            if let Ok(s) = status {
                if s.success() && Path::new(&res_file).exists() {
                    println!("cargo:rustc-link-arg-bin=puwbar-setup={}", res_file.display());
                }
            }
        }
    }
}
