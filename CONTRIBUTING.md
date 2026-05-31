# Contributing to Linux-Patch-Api

Thank you for your interest in contributing to Linux-Patch-Api! We appreciate every contribution — from bug reports and documentation improvements to new features and security fixes.

## Code of Conduct

This project follows the [Contributor Covenant v2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/) code of conduct. By participating, you are expected to uphold this standard. Please report unacceptable behavior to the maintainers.

## How to Contribute

1. **Fork** the repository
2. Create a **feature branch** from `main`:
   ```bash
   git checkout -b feat/my-feature
   ```
3. Make your changes
4. Ensure all CI checks pass:
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   ```
5. **Commit** using conventional commit format (see below)
6. Open a **Pull Request** against `main`

## Development Setup

### Prerequisites

- **Rust toolchain** (stable) — [rustup](https://rustup.rs/)
- **System dependencies**:
  ```bash
  sudo apt-get install build-essential libsystemd-dev pkg-config libssl-dev
  ```

### Build & Run

```bash
cargo build
cargo test
```

## Commit Messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

| Prefix   | Usage                  |
|----------|------------------------|
| `feat:`  | New feature            |
| `fix:`   | Bug fix                |
| `docs:`  | Documentation changes  |
| `chore:` | Maintenance tasks      |
| `refactor:` | Code refactoring    |
| `test:`  | Adding or updating tests |
| `ci:`    | CI configuration changes |

Example:
```
feat: add endpoint for patch rollback
```

## Pull Request Requirements

- All CI checks must pass (fmt, clippy, test, audit, build)
- One feature or fix per PR — keep changes focused
- Include a clear description of what changed and why
- Update documentation if your change affects behavior

## Reporting Issues

Use [GitHub Issues](https://github.com/Draco-Lunaris/Linux-Patch-Api/issues) to report bugs, request features, or ask questions. Please include:

- Steps to reproduce (for bugs)
- Expected vs. actual behavior
- Relevant logs or error messages

## License

By contributing, you agree that your contributions are licensed under the [Apache License 2.0](LICENSE), the same license as this project.
