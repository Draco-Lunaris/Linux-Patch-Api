# Security Policy

## Supported Versions

Only the **latest release** is currently supported with security updates.

| Version | Supported |
|---------|----------|
| Latest  | ✅       |
| Older   | ❌       |

## Reporting a Vulnerability

**Do not report security vulnerabilities through public GitHub Issues.**

Instead, use GitHub's private vulnerability reporting:

👉 [Report a vulnerability for Linux-Patch-Api](https://github.com/Draco-Lunaris/Linux-Patch-Api/security/advisories/new)

This allows us to coordinate a fix before public disclosure.

### Response Timeline

- **Acknowledgment** within 48 hours
- **Initial assessment** within 7 days
- **Ongoing updates** on remediation progress

## Disclosure Policy

We follow **coordinated disclosure**:

- We ask for **90 days** before public disclosure of a vulnerability
- Security advisories are published via [GitHub Security Advisories](https://github.com/Draco-Lunaris/Linux-Patch-Api/security/advisories)
- We will work with you to determine an appropriate disclosure timeline when a fix requires more time

## Security Best Practices

This project is a security tool — we hold ourselves to a high standard:

- **Signed commits**: All commits must be signed (SSH signing)
- **CI enforcement**: All PRs require passing CI checks (fmt, clippy, test, audit, build)
- **Dependency auditing**: `cargo audit` runs in CI to catch known vulnerabilities

## Credit

Contributors who responsibly report vulnerabilities will be credited in the corresponding GitHub Security Advisory.
