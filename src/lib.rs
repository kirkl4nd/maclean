//! maclean — find and reclaim disk space on macOS.
//!
//! `core` is the product. `cli` and `tui` are wrappers around the same
//! types: [`Item`], [`Module`], [`Registry`].

#[cfg(not(target_os = "macos"))]
compile_error!("maclean only supports macOS");

pub mod cli;
pub mod core;
pub mod modules;
pub mod schedule;
pub mod tui;

pub use core::{
    Item, Module, ModuleInfo, ModuleScan, ReclaimContext, ReclaimResult, Registry, Relevance,
    Safety, ScanContext, ScanEvent, ScanIssue, format_bytes, running_as_root,
};

pub use cli::run;
