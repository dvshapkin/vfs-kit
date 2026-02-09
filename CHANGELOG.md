Changelog
=========

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.12] - 2026-02-06

### Added
- new abstraction `DirEntry`
- `forget()` method for DirFS (excludes an existing file or directory from VFS)
- `ls()` method for `FsBackend` and realized it in DirFS
- `tree()` method for `FsBackend` and realized it in DirFS

### Changed
- `add()` if artifact is directory - all its childs will be added recursively
- inner implementation of `DirFS` for performance optimization in `exists()`, `mkdir()`, `rm()`, `cleanup()` and others

## [0.1.11] - 2026-02-05

### Fixed
- Documentation improved

### Added
- `add()` method for DirFS (adds an existing file or directory)

## [0.1.10] - 2026-02-04

### Fixed
- Fixed known bugs

### Added
- `read()`, `write()` and `append()` methods for DirFS

### Changed
- `FsBackend` trait definition
