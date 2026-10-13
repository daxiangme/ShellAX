// Probe: store/read a dummy secret through SecretStore and report which backend held it.
fn main() {
    let dir = std::env::temp_dir().join("as-keyring-probe");
    let st = ssh_term_proto::secrets::SecretStore::new(&dir);
    let (loc, why) = st.set("ai-provider:probe", "sk-probe-0000-WXYZ").expect("set");
    println!("stored: {loc:?} fallback_reason={why:?}");
    println!("read back ok: {}", st.get("ai-provider:probe").map(|x| x.0 == "sk-probe-0000-WXYZ").unwrap_or(false));
    if std::env::args().any(|a| a == "--keep") { return; }
    st.delete("ai-provider:probe");
    println!("after delete: {:?}", st.get("ai-provider:probe").map(|x| x.1));
}
