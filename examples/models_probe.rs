// Real-network check: list OpenRouter's public model catalog (no key needed) and enrich from models.dev.
use ssh_term_proto::{ai::Provider, models};
fn main() {
    let p = Provider { id: "or".into(), base_url: "https://openrouter.ai/api/v1".into(), preset: "openrouter".into(), timeout_secs: 30, ..Default::default() };
    let t = std::time::Instant::now();
    let mut m = models::list_models(&p, "").expect("list");
    println!("openrouter: {} models in {} ms; chat-only {}", m.len(), t.elapsed().as_millis(), models::filter_chat(&m, true).len());
    for x in m.iter().filter(|x| x.id.starts_with("deepseek/")).take(3) { println!("  {} ctx={:?} in=${:?}/M out=${:?}/M", x.id, x.context, x.price_in, x.price_out); }
    let dir = std::env::temp_dir().join("as-models-probe");
    let now = chrono::Utc::now().timestamp();
    let meta = models::models_dev(&dir, now, 86400).expect("models.dev");
    println!("models.dev entries: {}", meta.len());
    let mut ds = vec![models::ModelInfo { id: "deepseek-chat".into(), name: String::new(), context: None, price_in: None, price_out: None, usage: models::Use::Chat }];
    models::enrich(&mut ds, "deepseek", &meta);
    println!("deepseek-chat via models.dev: ctx={:?} in={:?} out={:?}", ds[0].context, ds[0].price_in, ds[0].price_out);
    m.clear();
}
