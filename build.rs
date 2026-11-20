use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn find_vcvars64() -> Option<PathBuf> {
    let vswhere =
        Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    if !vswhere.exists() {
        return None;
    }
    let out = Command::new(vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if root.is_empty() {
        return None;
    }
    let vcvars = PathBuf::from(root).join(r"VC\Auxiliary\Build\vcvars64.bat");
    vcvars.exists().then_some(vcvars)
}

fn main() {
    println!("cargo:rerun-if-changed=tap/puwbar_tap.cpp");
    println!("cargo:rerun-if-changed=tap/puwbar_tap.def");
    println!("cargo:rerun-if-changed=assets/puwbar.rc");
    println!("cargo:rerun-if-changed=assets/puwbar.ico");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let cpp = manifest_dir.join("tap").join("puwbar_tap.cpp");
    let def = manifest_dir.join("tap").join("puwbar_tap.def");
    let rc = manifest_dir.join("assets").join("puwbar.rc");
    let res = out_dir.join("puwbar.res");
    let obj = out_dir.join("puwbar_tap.obj");

    let Some(vcvars) = find_vcvars64() else {
        println!("cargo:warning=Không tìm thấy vcvars64.bat để build puwbar_tap.dll");
        return;
    };

    let opt = if env::var("PROFILE").as_deref() == Ok("release") {
        "/O2"
    } else {
        "/Od"
    };
    let built_dll = out_dir.join("puwbar_tap.dll");
    let bat = out_dir.join("build_tap.bat");
    let script = format!(
        "@echo off\r\ncall \"{}\" >nul\r\nrc.exe /nologo /fo \"{}\" \"{}\" || exit /b 1\r\ncl.exe /nologo /std:c++20 /EHsc {} /MT /LD /DUNICODE /D_UNICODE \"{}\" /Fe:\"{}\" /Fo:\"{}\" /link /DEF:\"{}\" windowsapp.lib ole32.lib user32.lib dxguid.lib\r\n",
        vcvars.display(),
        res.display(),
        rc.display(),
        opt,
        cpp.display(),
        built_dll.display(),
        obj.display(),
        def.display()
    );
    std::fs::write(&bat, script).expect("không ghi được build_tap.bat");

    let status = Command::new("cmd.exe")
        .arg("/C")
        .arg(&bat)
        .status()
        .expect("không chạy được cmd.exe để build puwbar_tap.dll");
    if !status.success() {
        panic!("Build puwbar_tap.dll hoặc resource icon thất bại.");
    }
    println!("cargo:rustc-link-arg-bin=puwbar={}", res.display());
}
