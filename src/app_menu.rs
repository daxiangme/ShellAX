//! Native application menu bar.
//!
//! macOS draws the app menu at the top of the screen: without one the app shows a bare
//! "ShellAX" menu with nothing in it, and ⌘Q does nothing because AppKit only maps ⌘Q when the
//! menu has a matching item. Windows and Linux paint their own chrome instead (`chrome.rs`), so
//! [`install`] returns immediately there and leaves the window untouched.
//!
//! Menu items dispatch GPUI actions; `Root` (see `main.rs`) turns each into the same
//! [`crate::actions::Action`] the keyboard and command palette already use, so a command has one
//! implementation no matter how it is triggered.

use gpui::{actions, App, KeyBinding, Menu, MenuItem, SystemMenuType};
use ssh_term_proto::i18n::t;

actions!(shellax_menu, [
    QuitApp,
    OpenSettings,
    OpenAbout,
    NewTerminalTab,
    NewHostDialog,
    SplitRightPane,
    SplitDownPane,
    ClosePane,
    OpenSftp,
    CopySelection,
    PasteClipboard,
    ToggleSidebar,
    CycleTheme,
    OpenPalette,
    OpenAiPanel,
    OpenSnippets,
    OpenHistory,
    OpenDirectory,
    OpenMcp,
    OpenAuditLog,
    MinimizeWindow,
    HideApp,
    HideOthers,
]);

/// Install the menu bar and the macOS shortcuts that go with it.
///
/// Called once from `main` after the config (and therefore the UI language) is loaded.
pub fn install(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    // Bind before set_menus: the platform reads the keymap to print shortcuts next to the items.
    cx.bind_keys(key_bindings());
    cx.set_menus(menus());
    cx.on_action(|_: &HideApp, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
}

/// Rebuild the menu bar, e.g. after the UI language changes in Settings.
pub fn refresh(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.set_menus(menus());
}

fn menus() -> Vec<Menu> {
    vec![
        // The first menu is the application menu: macOS shows its name as the bold title.
        Menu::new("ShellAX").items([
            MenuItem::action(t("menu.about"), OpenAbout),
            MenuItem::separator(),
            MenuItem::action(t("menu.settings"), OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu(t("menu.services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(t("menu.hide"), HideApp),
            MenuItem::action(t("menu.hide_others"), HideOthers),
            MenuItem::separator(),
            MenuItem::action(t("menu.quit"), QuitApp),
        ]),
        Menu::new(t("menu.file")).items([
            MenuItem::action(t("ui.new_local_terminal_tab"), NewTerminalTab),
            MenuItem::action(t("ui.new_host_2"), NewHostDialog),
            MenuItem::separator(),
            MenuItem::action(t("ui.split_left_right"), SplitRightPane),
            MenuItem::action(t("ui.split_top_bottom"), SplitDownPane),
            MenuItem::action(t("ui.close_current_pane"), ClosePane),
            MenuItem::separator(),
            MenuItem::action(t("ui.sftp_files"), OpenSftp),
        ]),
        Menu::new(t("menu.edit")).items([
            MenuItem::action(t("menu.copy"), CopySelection),
            MenuItem::action(t("menu.paste"), PasteClipboard),
        ]),
        Menu::new(t("menu.view")).items([
            MenuItem::action(t("ui.show_hide_sidebar"), ToggleSidebar),
            MenuItem::action(t("ui.toggle_theme"), CycleTheme),
            MenuItem::separator(),
            MenuItem::action(t("ui.command_palette"), OpenPalette),
            MenuItem::separator(),
            MenuItem::action(t("ui.ai_agent_panel"), OpenAiPanel),
            MenuItem::action(t("lib.snippets"), OpenSnippets),
            MenuItem::action(t("lib.history"), OpenHistory),
            MenuItem::action(t("dir.title"), OpenDirectory),
            MenuItem::action(t("mcp.title"), OpenMcp),
            MenuItem::action(t("ui.audit_log"), OpenAuditLog),
        ]),
        Menu::new(t("menu.window")).items([MenuItem::action(t("win.minimize"), MinimizeWindow)]),
        Menu::new(t("menu.help")).items([
            MenuItem::action(t("ui.settings_keyboard_shortcuts"), OpenSettings),
            MenuItem::action(t("menu.about"), OpenAbout),
        ]),
    ]
}

/// Shortcuts macOS users expect, and the ones the menu prints next to each item.
///
/// These are only bound on macOS; the Ctrl-based bindings in `actions.rs` keep working there too.
fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", QuitApp, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-t", NewTerminalTab, None),
        KeyBinding::new("cmd-w", ClosePane, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-shift-p", OpenPalette, None),
        KeyBinding::new("cmd-shift-t", CycleTheme, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
    ]
}
