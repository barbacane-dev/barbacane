# Security Policy

## Reporting a Vulnerability

We take security seriously. If you discover a security vulnerability in Barbacane, please report it responsibly.

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, please email us at: **security@barbacane.dev**

### What to Include

When reporting a vulnerability, please include:

- A description of the vulnerability
- Steps to reproduce the issue
- Potential impact of the vulnerability
- Any suggested fixes (if you have them)

### Response Timeline

- **Initial Response**: Within 48 hours, we will acknowledge receipt of your report
- **Assessment**: Within 7 days, we will provide an initial assessment
- **Resolution**: We aim to resolve critical vulnerabilities within 30 days

### Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

As the project matures, we will update this table to reflect our security support policy.

### Disclosure Policy

- We will work with you to understand and resolve the issue
- We will credit you in the security advisory (unless you prefer to remain anonymous)
- We ask that you give us reasonable time to address the issue before public disclosure

### Scope

This security policy applies to:

- The Barbacane gateway (all crates in the `crates/` directory)
- Official plugins in the `plugins/` directory
- The control plane and web UI

### Out of Scope

- Issues in third-party dependencies (please report these to the respective projects)
- Issues that require physical access to the server
- Social engineering attacks

## Security Best Practices

When deploying Barbacane:

1. **Keep Barbacane updated** to the latest version
2. **Use TLS** for all external communications
3. **Follow the principle of least privilege** when configuring plugins
4. **Review your OpenAPI specs** for unintended endpoint exposure
5. **Use secret management** (environment variables or file-based secrets) instead of hardcoding credentials

## Contact

For general security questions: security@barbacane.dev

For trademark and other inquiries: contact@barbacane.dev
