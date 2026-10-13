//! ssh-term-proto core: backend-agnostic terminal session (alacritty_terminal grid)
//! fed by either a local PTY (portable-pty) or an SSH channel (russh).
pub mod local;
pub mod ssh;
pub mod suggest;
pub mod term;
