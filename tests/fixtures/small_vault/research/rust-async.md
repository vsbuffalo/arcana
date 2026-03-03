---
title: Rust Async Programming
tags:
  - rust
  - programming
  - async
created: 2024-01-15T10:00:00Z
---
# Rust Async Programming

Async/await in Rust is built on top of futures. The `tokio` runtime is the most popular choice.

## Key Concepts

- **Futures** are lazy - they don't execute until polled
- **async fn** returns an `impl Future`
- **await** drives a future to completion
- Use `tokio::spawn` for concurrent tasks

#deep-dive #performance

See also [[rust-error-handling]] and [[tokio-internals]].
