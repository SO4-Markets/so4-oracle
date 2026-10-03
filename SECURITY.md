# Security Policy

SO4 Oracle is a core infrastructure component driving real-time price feeds, liquidations, and settlement for SO4 Markets on Stellar/Soroban. We take security seriously and appreciate prompt, responsible disclosure of potential vulnerabilities.

## Supported Versions

| Version / Branch | Supported | Notes |
| ---------------- | --------- | ----- |
| `main`           | Yes       | Latest production release branch |
| `< v1.0`         | No        | Legacy testnet releases |

## Reporting a Vulnerability

> [!IMPORTANT]
> **Do NOT file public GitHub issues for security vulnerabilities.** Publicly disclosing live keeper key vectors, price-manipulation bugs, or unauthorized transaction submission paths before a fix is deployed exposes the protocol and user funds to risk.

### Private Disclosure Channels

Please report security vulnerabilities privately via one of the following methods:

1. **GitHub Private Vulnerability Reporting:** Use the **"Report a vulnerability"** button under the **Security** tab of the `SO4-Markets/so4-oracle` repository.
2. **Email Disclosure:** Contact security@so4markets.io with details of the vulnerability, steps to reproduce, and any proof-of-concept code.

### Response SLA & Expectations

- **Initial Response:** Within **24 hours** of submission.
- **Triage & Severity Assessment:** Within **48 hours**.
- **Fix & Patch Deployment:** Critical vulnerabilities are prioritized for emergency patch deployment within **72 hours**.

## Scope & Out-of-Scope

### In-Scope
- Private key disclosure or compromise vectors in keeper execution.
- Price feed manipulation, cache poisoning, or oracle spoofing.
- Transaction auth-bypass or unauthorized execution paths.
- Denial-of-service vectors targeting keeper cycle execution.

### Out-of-Scope
- Vulnerabilities dependent on unreleased testnet-only features.
- Social engineering, phishing, or physical attacks targeting infrastructure operators.
- Issues already publicly disclosed.

## Disclosure Policy

Once a fix has been tested and merged, we will coordinate with the reporter on public disclosure and credit in security release notes.
