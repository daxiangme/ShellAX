//! Test helper: store a secret the way the app does. Usage: set_secret <data_dir> <name> <value>
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let s = ssh_term_proto::secrets::SecretStore::file_only(&a[0]);
    println!("{:?}", s.set(&a[1], &a[2]).map(|r| r.0));
}
