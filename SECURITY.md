# Security Policy

## Reporting a Vulnerability

Please report security issues privately first. Include:

- affected component (`core` / `agent` / `hub` / deployment)
- reproduction steps
- impact scope
- suggested mitigation (if available)

Do not open public issues for active vulnerabilities until a fix is available.

## Security Notes

- `agent` may run with elevated permissions in Kubernetes; deploy only in trusted clusters.
- Default remediation must remain conservative (`dry-run` before execute).
- Review RBAC scopes before enabling auto quarantine.
