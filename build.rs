// Windows only: compile packaging/windows/shellax.rc (app icon + version info) into a resource
// and link it into the GUI binary.
//   MSVC targets: llvm-rc (cross builds) or rc.exe (native) → a .res file.
//   GNU targets:  windres (MinGW) → a COFF object with a .rsrc section.
// Without a resource compiler the build still succeeds; the exe just has no icon and no
// version info (cargo prints a warning), which is why the packaged exe used to look generic.
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/shellax.rc");
    println!("cargo:rerun-if-changed=packaging/windows/shellax.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") { return; }
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/windows");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        let out = out_dir.join("shellax.res");
        for rc in ["llvm-rc", "rc"] {
            let ok = std::process::Command::new(rc).current_dir(&dir)
                .args(["/nologo", "/fo"]).arg(&out).arg("shellax.rc")
                .status().map(|s| s.success()).unwrap_or(false);
            if ok {
                println!("cargo:rustc-link-arg-bin=shellax={}", out.display());
                return;
            }
        }
    } else if target_env == "gnu" {
        // windres only emits the architecture it was configured for; skip 32-bit targets rather
        // than hand the linker a mismatched resource object.
        if std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() != Ok("64") { return; }
        let out = out_dir.join("shellax-res.o");
        let _ = std::fs::remove_file(&out);
        for rc in ["windres", "x86_64-w64-mingw32-windres"] {
            let ok = std::process::Command::new(rc).current_dir(&dir)
                .args(["-F", "pe-x86-64", "-O", "coff", "-o"]).arg(&out).arg("shellax.rc")
                .status().map(|s| s.success()).unwrap_or(false);
            if ok && out.exists() {
                println!("cargo:rustc-link-arg-bin=shellax={}", out.display());
                return;
            }
        }
    } else {
        return;
    }
    println!("cargo:warning=no resource compiler found; Windows exe will have no icon");
}
