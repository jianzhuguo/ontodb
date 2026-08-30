# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.6.x   | :white_check_mark: |

## Reporting a Vulnerability

**Please do NOT open a public GitHub issue for security vulnerabilities.**

Instead, report them privately via email to: **security@ontodb.ai**.

### What to include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response timeline

- **Acknowledgment**: within 48 hours
- **Initial assessment**: within 1 week
- **Fix release**: depends on severity, target within 2 weeks for critical issues

## Security Best Practices for Deployment

1. **Never commit secrets** â€?`.env`, `config/api_keys.json`, TLS keys are in `.gitignore`
2. **Use TLS in production** â€?see `deploy/nginx/` for TLS proxy configuration
3. **Rotate API keys** â€?use `config/api_keys.example.json` as a template, rotate keys regularly
4. **Rate limiting** â€?enabled by default (300 req/min), adjust via `ONTODB_RATE_LIMIT`
5. **Network isolation** â€?bind to `127.0.0.1` by default; expose `0.0.0.0` only behind a reverse proxy
