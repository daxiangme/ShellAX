//! ssh-term-proto core: backend-agnostic terminal session (alacritty_terminal grid)
//! fed by either a local PTY (portable-pty) or an SSH channel (russh).
pub mod agent;
pub mod ai;
pub mod models;
pub mod i18n;
pub mod secrets;
pub mod audit;
pub mod policy;
pub mod hooks;
pub mod policy_server;
pub mod boxdraw;
pub mod hosts;
pub mod keygen;
pub mod layout;
pub mod local;
pub mod ssh;
pub mod update;
pub mod sftp;
#[cfg(unix)]
pub mod daemon;
pub mod session_store;
pub mod forward;
pub mod suggest;
pub mod term;
