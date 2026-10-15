//! System tray (PRD 3.6 P1: "托盘图标显示运行中的 Agent 数"). Linux: StatusNotifierItem over
//! D-Bus via `ksni` (Unlicense). The tray shows the number of running agents and offers
//! Show window / New local terminal / Quit; clicks are sent to the UI over a channel.
//! When no StatusNotifierWatcher is running (bare X11 without a panel), the tray is skipped.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd { Show, NewLocal, Quit }

#[derive(Debug)]
pub struct AppTray { pub agents: usize, pub waiting: usize, pub zh: bool, pub tx: flume::Sender<TrayCmd> }

impl AppTray {
    pub fn summary(&self) -> String {
        match (self.zh, self.agents, self.waiting) {
            (true, 0, _) => "ShellAX · 没有运行中的 Agent".into(),
            (true, n, 0) => format!("ShellAX · {n} 个 Agent 运行中"),
            (true, n, w) => format!("ShellAX · {n} 个 Agent 运行中（{w} 个等待输入）"),
            (false, 0, _) => "ShellAX · no agents running".into(),
            (false, n, 0) => format!("ShellAX · {n} agent{} running", if n == 1 { "" } else { "s" }),
            (false, n, w) => format!("ShellAX · {n} agent{} running ({w} waiting for input)", if n == 1 { "" } else { "s" }),
        }
    }
    pub fn labels(&self) -> [&'static str; 3] {
        if self.zh { ["显示窗口", "新建本地终端", "退出"] } else { ["Show window", "New local terminal", "Quit"] }
    }
}

#[cfg(target_os = "linux")]
impl ksni::Tray for AppTray {
    fn id(&self) -> String { "shellax".into() }
    fn title(&self) -> String { self.summary() }
    fn icon_name(&self) -> String { "utilities-terminal".into() }
    fn status(&self) -> ksni::Status { if self.waiting > 0 { ksni::Status::NeedsAttention } else { ksni::Status::Active } }
    fn tool_tip(&self) -> ksni::ToolTip { ksni::ToolTip { title: self.summary(), description: String::new(), icon_name: String::new(), icon_pixmap: vec![] } }
    fn activate(&mut self, _x: i32, _y: i32) { let _ = self.tx.send(TrayCmd::Show); }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let [a, b, c] = self.labels();
        let item = |label: &str, cmd: TrayCmd| StandardItem { label: label.into(), activate: Box::new(move |t: &mut Self| { let _ = t.tx.send(cmd); }), ..Default::default() }.into();
        vec![
            StandardItem { label: self.summary(), enabled: false, ..Default::default() }.into(),
            ksni::MenuItem::Separator,
            item(a, TrayCmd::Show), item(b, TrayCmd::NewLocal), ksni::MenuItem::Separator, item(c, TrayCmd::Quit),
        ]
    }
    // no watcher (yet): keep the service alive so a panel started later still picks it up
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool { true }
}

/// Handle used by the UI to update the agent count.
#[cfg(target_os = "linux")]
pub type Handle = ksni::Handle<AppTray>;

/// Start the tray on the tokio runtime. Errors (no session bus) are returned for logging.
#[cfg(target_os = "linux")]
pub async fn spawn(t: AppTray) -> anyhow::Result<Handle> {
    use ksni::TrayMethods;
    t.spawn().await.map_err(|e| anyhow::anyhow!("tray unavailable: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_and_labels() {
        let (tx, _rx) = flume::unbounded();
        let mut t = AppTray { agents: 0, waiting: 0, zh: true, tx };
        assert!(t.summary().contains("没有"));
        t.agents = 2; t.waiting = 1;
        assert_eq!(t.summary(), "ShellAX · 2 个 Agent 运行中（1 个等待输入）");
        t.zh = false; t.waiting = 0; t.agents = 1;
        assert_eq!(t.summary(), "ShellAX · 1 agent running");
        assert_eq!(t.labels()[2], "Quit");
    }
}
