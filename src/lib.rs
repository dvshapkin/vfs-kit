//! A lightweight, extensible virtual filesystem (VFS) toolkit for Rust. Provides in‑process abstractions over real
//! or simulated filesystems, ideal for testing, sandboxing, custom storage backends, and so on.
//! 
//! ## Overview
//! 
//! `vfs-kit` lets you work with filesystem-like structures in Rust without touching the real disk (unless you want to).
//! It defines a common `FsBackend` trait and provides concrete implementations like `DirFS` that map to real directories.
//! 
//! **Key ideas**:
//! - **Abstraction**: Treat different storage backends (real dirs, memory maps, etc.) via a unified API.
//! - **Safety**: Operations are confined to a root path; no accidental host filesystem access.
//! - **Testability**: Use in unit tests to simulate filesystems without side effects.
//! - **Extensibility**: Plug in new backends by implementing `FsBackend`.
//! - **Clarity**: Comprehensive error messages and documentation.
//! 
//! ## Features
//! 
//! - Path normalization (`.`, `..`, trailing slashes)
//! - Current working directory (`cwd`) support
//! - Create/read/write/remove files and directories
//! - Existence checks and state tracking
//! - Auto‑cleanup on drop (optional)
//! - Cross‑platform path handling
//! - Rich error messages via `anyhow`
//! - Clean, documented API
//! - Easy to extend with custom backends

mod core;
mod vfs;

pub use core::{Result, FsBackend};
pub use vfs::DirFS;