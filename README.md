Virtual File System Toolkit
===========================

[![Latest version](https://img.shields.io/crates/v/vfs-kit.svg)](https://crates.io/crates/vfs-kit)
![License](https://img.shields.io/crates/l/vfs-kit.svg)
[![Tests](https://github.com/dvshapkin/vfs-kit/actions/workflows/ci.yaml/badge.svg)](https://github.com/dvshapkin/vfs-kit/actions/workflows/ci.yaml)
[![Documentation](https://docs.rs/vfs-kit/badge.svg)](https://docs.rs/vfs-kit)

A lightweight, extensible set of virtual file systems (VFS) for Rust.
Provides abstractions over real or pseudo-file systems. Ideal for testing,
isolated sandboxing, custom storage backends, and more.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
vfs-kit = "0.2"
```

Or via `cargo add`:

```bash
cargo add vfs-kit
```

## What's new in last version?
### [0.2.0]
### Added
- `MapFS` implementation
- new method `FsBackend::to_host()`
- new method `FsBackend::is_dir()`
- new method `FsBackend::is_file()`
### Changed
+ `ls()` and `tree()` - the item type in the iterator is now `&Path`
### Fixed
- `cd()` - returns error now if target path is a file
- Fixed known bugs
- Inaccuracies in the documentation have been corrected


## Overview

`vfs-kit` allows you to work with filesystem-like structures in Rust without touching the actual disk (unless you want to). 
It defines the generic `FsBackend` trait and provides specific implementations, such as `DirFS` (which maps to real directories)
and `MapFS`.

**Key ideas**:
- **Abstraction**: Work with different types of storage (real directories, memory cards, etc.) through a single API.
- **Safety**: Operations are performed only within the VFS root directory; random access to the host file system is excluded.
- **Testability**: Use in unit tests to simulate filesystems without side effects.
- **Extensibility**: Create your own storages by adding new `FsBackend` implementations.
- **Clarity**: Detailed error messages and up-to-date documentation.

## How does this work?

Let me explain the logic of working with `DirFS` using a typical example.

Suppose your project requires creating a specific structure of real directories and files for testing purposes.
Suppose your application is cross-platform, and therefore your tests and the directories and files they create should run 
successfully on any OS (Linux, Windows, MacOS). Furthermore, after completing the tests, all created directories and files 
should be deleted (optional) to avoid creating side effects on the host system. This should not affect the host's data.
In this case, `DirFS` is what you need.

### Creating a new isolated VFS
```
let mut fs = DirFS::new("/absolute/path/to/vfs/root");
```
You can specify an existing or non-existent directory on the host system as the root of the new VFS.
If the specified directory doesn't exist, it will be created.

**Important**: When selecting the root directory, you must ensure that the VFS has sufficient access rights to create it and further work with it!

#### Example 1:
Let's say your host has a directory named `/home/user` with create, read, and write permissions.
Then, running the command
```
let mut fs = DirFS::new("/home/user/tests/root");
```
This will create two new subdirectories: `tests` and `root`.
The `/home/user/tests/root` directory will become the root of the created VFS.

**Note**: Since `DirFS` deletes everything created within its "jurisdiction" when it terminates, 
the `tests` and `root` directories will be deleted as expected. However, the '/home/user' directory will remain.

In addition to the root, `DirFS` internally stores the current working directory (CWD) in relative form 
(i.e., a short path relative to the root). Immediately after creating the `DirFS`, the CWD value is `/`.
You can obtain the root and CWD values using:
```
fs.root()   // returns the absolute path to the root on the host system
fs.cwd()    // returns the current working directory inside the VFS ("internal path")
```

You can explicitly change the current working directory within the VFS using the `cd()` command.

#### Example 2:
Let's say your host has a directory named `/home/user/work` with create, read, and write permissions,
and it already contains three files: `file.01`, `file.02`, `file.03`.
Then, running the command
```
let mut fs = DirFS::new("/home/user/work");
```
This will create a new VFS rooted in the `/home/user/work` directory. However, existing files in it will not be visible
within the VFS. This is done to protect host data. However, you can explicitly add all (or just some) of them to the VFS's control
using the following commands:
```
fs.add("/file.01")  // absolute path used
fs.add("file.02")   // relative path used
fs.add("/")         // will add all root contents (all files and subdirectories) under VFS control recursively
```
As the example shows, if you pass a directory as a parameter to the `add()` function, the VFS will manage all of its contents (recursively).
Also, note that `add()` accepts the "internal VFS path" as the path, meaning a path relative to the VFS root.
This is convenient, as such a path can be significantly shorter than the full path on the host system. 
It can be written in either absolute or relative form. If the path is relative, the VFS automatically converts it 
to absolute form by concatenating it with the current working directory, i.e.:
```
absolute_path = CWD + related_path
```
This rule of converting a relative path to an absolute one (inside VFS) works for almost all `DirFS` functions.

**Note**: `DirFS` also allows to inverse of `add()` operation with `forget()`, to remove certain files/directories from its control 
(without deleting them from the host system).

### Creating nested files and directories within VFS

#### Example 3:
Let's say you have a directory `/home/user/work` on your host, and inside it there are already three files: `file.01`, `file.02`, `file.03`. (déjà vu...)

Let's create a new VFS:
```
let mut fs = DirFS::new("/home/user/work");
```
Let's create some new files and directories inside the VFS:
```
fs.mkfile("new_file.01", None);             // will create a new empty file inside the VFS
                                            // the path inside VFS will be: /new_file.01
                                            // the path on the host system will be: /home/user/work/new_file.01
                                            
fs.mkdir("subdir")                          // will create the directory /subdir
fs.cd("subdir")                             // now CWD = /subdir

fs.mkfile("new_file.02", b"Hello world");   // will create a new file with the contents inside VFS
                                            // the path inside VFS will be: /subdir/new_file.02
                                            // the path on the host system will be: /home/user/work/subdir/new_file.02
                                            
fs.mkfile("/file.03", None)                  // ERROR: such a file already exists, although it is not visible in VFS

fs.add("/file.02")                           // OK: add an existing file to VFS
fs.add("/file.03")                           // OK: add an existing file to VFS

fs.forget("/file.02")                        // remove the file from VFS control (but not from the disk!)

// At the end of the scope, the 'fs' variable is destroyed, and the drop() function physically removes files 
// and directories that are under VFS control from the host system's disk (if flag 'is_auto_clean' == true).
// In this case, these are: new_file.01, /subdir/new_file.02, the /subdir directory, and file.03.
```

### What else can be done?
+ Check for the existence of a file or directory in the VFS using `exists()`
+ Read `read()`, overwrite `write()` and append `append()` files with content
+ Remove individual files or entire directories with `rm()`
+ Iterate over directory contents with `ls()` and recursively with `tree()`
+ Clean the VFS with `cleanup()`

## What's different about `MapFS`?
`MapFS` doesn't work with the host filesystem at all (unlike `DirFS`). Instead of actual files and directories, 
pseudo-files and pseudo-directories are created, but the same API defined in `FsBackend` applies to them.
This means you can create directories and files, write contents to them, read from files, and so on. Since all 
the artifacts you create will be stored in RAM, `MapFS` isn't suitable for creating too many of them. 
This can slow down your application and the system as a whole. If you need a large number of files and/or directories, 
it's better to use `DirFS`.

### Optimal use scenarios
+ temporary data storage within a single process;
+ unit testing of file operations (without affecting the real FS);
+ caching small amounts of data with fast access;
+ prototyping file interactions;
+ working with configurations/templates in memory.

### Comparison with `DirFS`
+ `MapFS`:
  - speed of operations (memory vs disk);
  - isolation (does not affect the host FS);
  - limited by RAM capacity;
  - data is lost when the process terminates (serialization is planned to be implemented).
+ `DirFS`:
  - data persistence;
  - support for large volumes;
  - slower (I/O to disk);
  - risk of side effects on the host FS.

## API Summary

### Core Trait
* `FsBackend`: Defines the VFS interface:
  + `root()` — get the root path
  + `cwd()` — get current working directory
  + `to_host()` — returns the path on the host system that matches the specified internal path
  + `cd(path)` — change directory
  + `exists(path)` — check if path exists
  + `is_dir(path)` — check if path is a directory
  + `is_file(path)` — check if path is a regular file
  + `ls(path)` — returns an iterator over directory entries
  + `tree(path)` — returns a recursive iterator over the directory tree starting from a given path
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

## Planned Features

We’re working on these backends:
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