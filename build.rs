// Windows only: compile packaging/windows/agentshell.rc (app icon + version info) into a .res
// and link it into the GUI binary. Uses llvm-rc (cross builds) or rc.exe (native MSVC).
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/agentshell.rc");
    println!("cargo:rerun-if-changed=packaging/windows/agentshell.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") { return; }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("agentshell.res");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/windows");
    for rc in ["llvm-rc", "rc"] {
        let ok = std::process::Command::new(rc).current_dir(&dir)
            .args(["/nologo", "/fo"]).arg(&out).arg("agentshell.rc")
            .status().map(|s| s.success()).unwrap_or(false);
        if ok {
            println!("cargo:rustc-link-arg-bin=ssh-term-proto={}", out.display());
            return;
        }
    }
    println!("cargo:warning=no resource compiler found; Windows exe will have no icon");
}
