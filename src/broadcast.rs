//! Broadcast input: one keystroke stream to several panes. Production hosts in the target
//! set require an explicit confirmation before broadcasting starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Target { pub id: u64, pub label: String, pub production: bool }
#[derive(Debug, PartialEq)]
pub enum Start { Ok(Vec<u64>), NeedConfirm(Vec<String>), TooFew }
/// Decide whether broadcasting may start for `targets` (`confirmed` = user accepted the prod warning).
pub fn start(targets: &[Target], confirmed: bool) -> Start {
    if targets.len() < 2 { return Start::TooFew; }
    let prod: Vec<String> = targets.iter().filter(|t| t.production).map(|t| t.label.clone()).collect();
    if !prod.is_empty() && !confirmed { return Start::NeedConfirm(prod); }
    Start::Ok(targets.iter().map(|t| t.id).collect())
}
/// Banner text shown on every participating pane.
pub fn banner(n: usize, zh: bool) -> String { if zh { format!("广播输入中 · 同时发送到 {n} 个面板 · Ctrl+Shift+B 结束") } else { format!("BROADCAST · typing into {n} panes · Ctrl+Shift+B to stop") } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gating() {
        let a = Target { id: 1, label: "local".into(), production: false };
        let b = Target { id: 2, label: "db1".into(), production: true };
        assert_eq!(start(std::slice::from_ref(&a), false), Start::TooFew);
        assert_eq!(start(&[a.clone(), b.clone()], false), Start::NeedConfirm(vec!["db1".into()]));
        assert_eq!(start(&[a.clone(), b], true), Start::Ok(vec![1, 2]));
        assert_eq!(start(&[a.clone(), Target { id: 3, ..a }], false), Start::Ok(vec![1, 3]));
        assert!(banner(3, true).contains("3"));
    }
}
