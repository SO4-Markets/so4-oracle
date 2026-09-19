# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| `main`  | :white_check_mark: |

## Reporting a Vulnerability

The `so4-oracle` service interacts with live Stellar / Soroban networks and handles signing keys for oracle price publishing and keeper executions. We take security vulnerabilities seriously.

### How to Report

Please **do not** open public GitHub issues for security vulnerabilities or key-management concerns.

Instead, please report security issues privately through one of the following channels:

1. **GitHub Security Advisories**: Use the "Report a vulnerability" button under the **Security** tab of the repository.
2. **Email Disclosure**: Send details to `security@so4.markets` (or maintainers directly).

### What to Include

Please provide:
- A description of the vulnerability and its potential impact.
- Affected component(s) or file paths (e.g. `oracle/src/...`).
- Step-by-step instructions or proof-of-concept (PoC) to reproduce the issue.
- Any suggested remediation or patch.

### Response SLA

- **Initial Response:** Within 24-48 hours acknowledging receipt of the report.
- **Triage & Assessment:** Within 5 business days with an initial severity assessment.
- **Fix & Disclosure:** We will coordinate a patch release and advisory disclosure timeline with the reporter.

### Scope

- **In Scope:** Key extraction vectors, price manipulation/tampering vulnerabilities, unauthorized transaction signing or submission bypasses, denial of service on keeper/price loops.
- **Out of Scope:** Attacks requiring physical access to the host machine or pre-compromised infrastructure.
