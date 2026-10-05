//! Shared logic for the Erlkonig menu-bar app (`erlkonig`) and its node
//! supervisor (`erlkonig-node`).
//!
//! Nothing here links triblespace or faculties: the app talks to the bundled
//! `trible` and `faculties` executables as child processes.

pub mod config;
pub mod envscrub;
pub mod init;
pub mod install;
pub mod logs;
pub mod paths;
pub mod procs;
pub mod status;
pub mod supervise;
pub mod words;
