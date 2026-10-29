//! Test helper: a minimal org.kde.StatusNotifierWatcher (what a desktop panel provides), so the
//! tray can be verified on a bare X server. Prints every registered item. Not shipped.
use std::sync::{Arc, Mutex};

struct Watcher { items: Arc<Mutex<Vec<String>>> }

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(&self, service: String, #[zbus(header)] h: zbus::message::Header<'_>, #[zbus(signal_emitter)] em: zbus::object_server::SignalEmitter<'_>) {
        let sender = h.sender().map(|s| s.to_string()).unwrap_or_default();
        let id = if service.starts_with('/') { format!("{sender}{service}") } else { service };
        println!("registered {id}");
        self.items.lock().unwrap().push(id.clone());
        let _ = Self::status_notifier_item_registered(&em, &id).await;
    }
    async fn register_status_notifier_host(&self, _service: String) {}
    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> { self.items.lock().unwrap().clone() }
    #[zbus(property)]
    async fn is_status_notifier_host_registered(&self) -> bool { true }
    #[zbus(property)]
    async fn protocol_version(&self) -> i32 { 0 }
    #[zbus(signal)]
    async fn status_notifier_item_registered(em: &zbus::object_server::SignalEmitter<'_>, service: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(em: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let _c = zbus::connection::Builder::session()?
        .name("org.kde.StatusNotifierWatcher")?
        .serve_at("/StatusNotifierWatcher", Watcher { items: Default::default() })?
        .build().await?;
    println!("watcher ready");
    std::future::pending::<()>().await;
    Ok(())
}
