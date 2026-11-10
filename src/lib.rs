//! keep-vm-resolution: keeps a Windows VM guest at a preset screen resolution.
//!
//! VMware Tools resizes the guest display to fit the console window when someone
//! opens the vSphere web console (autofit). The background instance runs in the
//! interactive user session, notices the change and switches back.

#[macro_use]
pub mod log;
pub mod autostart;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod display;
