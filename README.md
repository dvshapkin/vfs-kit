Virtual File System Toolkit
===========================

[![Latest version](https://img.shields.io/crates/v/vfs-kit.svg)](https://crates.io/crates/vfs-kit)
![License](https://img.shields.io/crates/l/vfs-kit.svg)
[![Tests](https://github.com/dvshapkin/vfs-kit/actions/workflows/ci.yaml/badge.svg)](https://github.com/dvshapkin/vfs-kit/actions/workflows/ci.yaml)
[![Documentation](https://docs.rs/vfs-kit/badge.svg)](https://docs.rs/vfs-kit)

A lightweight, extensible virtual filesystem (VFS) toolkit for Rust. Provides in‑process abstractions over real 
or simulated filesystems, ideal for testing, sandboxing, custom storage backends, and so on.

## Overview

`vfs-kit` lets you work with filesystem-like structures in Rust without touching the real disk (unless you want to). 
It defines a common `FsBackend` trait and provides concrete implementations like `DirFS` that map to real directories.

**Key ideas**:
- **Abstraction**: Treat different storage backends (real dirs, memory maps, etc.) via a unified API.
- **Safety**: Operations are confined to a root path; no accidental host filesystem access.
- **Testability**: Use in unit tests to simulate filesystems without side effects.
- **Extensibility**: Plug in new backends by implementing `FsBackend`.
- **Clarity**: Comprehensive error messages and documentation.

## Features

- Path normalization (`.`, `..`, trailing slashes)
- Current working directory (`cwd`) support
- Create/read/write/remove files and directories
- Existence checks and state tracking
- Auto‑cleanup on drop (optional)
- Cross‑platform path handling
- Rich error messages via `anyhow`
- Clean, documented API
- Easy to extend with custom backends

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
vfs-kit = "0.1"
```

Or via `cargo add`:

```bash
cargo add vfs-kit
```

## Getting Started
1. Add `vfs-kit` to your `Cargo.toml`.
2. Choose a backend (`DirFS` for real dirs, plan `MapFS` for memory).
3. Create an instance with a root path.
4. Use `mkdir`, `mkfile`, `rm`, `cd`, `read`, `write`, `append` and `exists` as needed.
5. Let the VFS clean up on drop (or disable auto‑cleanup).

## Usage Example

```rust
use vfs_kit::{DirFS, FsBackend};
use std::path::Path;

fn main() -> anyhow::Result<()> {
    // Create a VFS rooted at a temporary directory
    let temp_dir = tempdir::TempDir::new("vfs_example")?;
    let mut fs = DirFS::new(temp_dir.path())?;

    // Make a directory
    fs.mkdir("/docs")?;

    // Write a file
    fs.mkfile("/docs/note.txt", Some(b"Hello, VFS!"))?;

    // Check existence
    assert!(fs.exists("/docs/note.txt"));

    // Remove it
    fs.rm("/docs/note.txt")?;
    assert!(!fs.exists("/docs/note.txt"));
    
    // On drop, temp_dir is cleaned up automatically (if is_auto_clean=true)
    Ok(())
}
```

## API Summary

### Core Trait
* `FsBackend`: Defines the VFS interface:
  + `root()` — get the root path
  + `cwd()` — get current working directory
  + `cd(path)` — change directory
  + `exists(path)` — check if path exists
  + `mkdir(path)` — creates directory
  + `mkfile(path, content)` — creates file with optional content
  + `read(path)` — read all contents of a file
  + `write(path, content)` — writes contents to a file
  + `append(path, content)` — appends content to the end of the file
  + `rm(path)` — removes file or directory (recursively)
  + `cleanup()` — removes all created artifacts (dirs and files)

### Implementations
* `DirFS`: Maps to a real directory on disk.
  + All operations are relative to root.
  + Tracks state.
  + Supports auto‑cleanup of created parent directories.
  + Normalizes paths automatically.
  + Enforces absolute root path at construction.

## Design Principles
- **Minimalism:** Only essential VFS operations.
- **Transparency:** Errors include context (e.g., which path failed).
- **Zero‑cost abstraction:** No runtime overhead beyond what the backend needs.
- **User‑first:** Clear docs, examples, and error messages.
- **Test‑friendly:** Designed for use in unit and integration tests.

## Planned Features

We’re working on these backends:
* `MapFS`
  + In‑memory filesystem using Map.
  + Ideal for testing and transient data.
  + No disk I/O; fully deterministic.
  + Great for mocking file content in tests.


* `LogFS`
  + Append‑only log‑structured filesystem.
  + Persists operations as a sequence of atomic log entries.
  + Useful for audit trails, replay, and crash recovery.
  + Configurable log rotation and compaction.


* **Additional Backends** (roadmap)
  + ZipFS: Read/write ZIP archives as VFS.
  + CloudFS: Mount remote HTTP/S3 resources.
  + EncryptedFS: Layered encryption over any backend.

## Contributing
We welcome:
* Bug reports
* Feature requests
* Documentation improvements

## Contact & Links
* Repository: https://github.com/dvshapkin/vfs-kit
* Issues: https://github.com/dvshapkin/vfs-kit/issues
* Documentation: https://docs.rs/vfs-kit