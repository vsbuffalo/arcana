---
title: Rust Error Handling
tags:
  - rust
  - programming
  - error-handling
---
# Rust Error Handling

The `Result<T, E>` type is the primary mechanism for recoverable errors in Rust.

## Patterns

- Use `?` operator for propagation
- `thiserror` for library errors
- `anyhow` for application errors
- Custom error types with `From` implementations

Related: [[rust-async]] for async error handling patterns.
