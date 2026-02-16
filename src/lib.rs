//! A lightweight, extensible set of virtual file systems (VFS) for Rust.
//! Provides abstractions over real or pseudo-file systems. Ideal for testing,
//! isolated sandboxing, custom storage backends, and more.
//!
//! ### Overview
//!
//! `vfs-kit` allows you to work with filesystem-like structures in Rust without touching the actual disk (unless you want to).
//! It defines the generic `FsBackend` trait and provides specific implementations, such as `DirFS`, which map to actual directories.
//!
//! **Key ideas**:
//! - **Abstraction**: Work with different types of storage (real directories, memory cards, etc.) through a single API.
//! - **Safety**: Operations are performed only within the VFS root directory; random access to the host file system is excluded.
//! - **Testability**: Use in unit tests to simulate filesystems without side effects.
//! - **Extensibility**: Create your own storages by adding new `FsBackend` implementations.
//! - **Clarity**: Detailed error messages and up-to-date documentation.

mod core;
mod vfs;

pub use core::{FsBackend, Result};
pub use vfs::{DirFS, Entry, EntryType, MapFS};
