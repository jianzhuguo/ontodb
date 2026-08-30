# Security

OntoDB provides multiple layers of security protection.

## API Key Authentication

### Enable Authentication

```bash
./ontodb-server --auth --api-key-file config/api_keys.json
```

### API Keys File Format

```json
{
  "keys": {
    "sk-abc123": {
      "name": "production-app",
      "permissions": ["read", "write"],
      "ip_restrictions": ["10.0.0.0/8", "192.168.1.0/24"]
    },
    "sk-def456": {
      "name": "readonly-monitor",
      "permissions": ["read"]
    }
  }
}
```

### Using API Keys

```bash
curl -H "X-API-Key: sk-abc123" http://localhost:7912/api/query \
  -d '{"query": "SELECT * FROM users"}'
```

### Admin Key Management

Manage API keys via HTTP API (requires admin key):

```bash
# List all keys
GET /api/admin/keys

# Add key
POST /api/admin/keys
{
  "key": "sk-new789",
  "name": "new-app",
  "permissions": ["read", "write"],
  "ip_restrictions": []
}

# Update key
PUT /api/admin/keys/sk-new789
{
  "name": "updated-name",
  "permissions": ["read"]
}

# Delete key
DELETE /api/admin/keys/sk-new789
```

## Rate Limiting

### Default Settings

- 60 requests/minute per API key
- Burst size: 10

### Configure

```bash
# Custom limits
./ontodb-server --rate-limit-rpm 1000 --rate-limit-burst 50

# Disable (development only)
./ontodb-server --no-rate-limit
```

## Security Headers

OntoDB automatically sets security headers on all responses:

| Header | Value | Purpose |
|--------|-------|---------|
| `X-Content-Type-Options` | `nosniff` | Prevent MIME sniffing |
| `X-Frame-Options` | `DENY` | Prevent clickjacking |
| `Content-Security-Policy` | `default-src 'self'` | Prevent XSS |
| `X-XSS-Protection` | `1; mode=block` | XSS filter |

## SQL Injection Protection

OntoDB validates all identifiers and filters:

### Identifier Validation

Only allowed characters: `[a-zA-Z0-9_.]`

Rejected:
- SQL keywords in identifiers
- Semicolons
- Comments (`--`, `/* */`)
- Special characters

### Filter Validation

Input filters are validated before use:

```bash
# Valid
{"filter": "category = 'electronics'"}

# Rejected (SQL injection attempt)
{"filter": "1; DROP TABLE users--"}
```

## CORS Configuration

```bash
# Allow specific origins
./ontodb-server --cors-origins "https://app.example.com,https://admin.example.com"

# Allow all (development only)
./ontodb-server --cors-origins "*"
```

## TLS/HTTPS

### With Reverse Proxy (Recommended)

```nginx
# Nginx configuration
server {
    listen 443 ssl;
    server_name ontodb.example.com;

    ssl_certificate /etc/ssl/certs/ontodb.pem;
    ssl_certificate_key /etc/ssl/private/ontodb.key;

    location / {
        proxy_pass http://127.0.0.1:7912;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

### Direct TLS (Experimental)

```bash
./ontodb-server --tls-cert cert.pem --tls-key key.pem
```

## Backup Security

### Path Validation

Backup paths are validated:
- Must be absolute paths
- No `..` traversal
- No special characters

### Encrypted Backups (Enterprise)

Enterprise edition supports AES-256-GCM encrypted backups:

```bash
curl -X POST http://localhost:7912/api/backup \
  -H "Authorization: Bearer admin-key" \
  -d '{"path": "/backups/secure", "encrypt": true}'
```

## Audit Logging (Enterprise)

Enterprise edition provides audit logging:

```bash
./ontodb-server --audit --audit-dir /var/log/ontodb/audit
```

Audit events:
- All query executions
- Authentication attempts
- Configuration changes
- Backup/restore operations

## RBAC (Enterprise)

Three-privilege separation:

| Role | Permissions |
|------|-------------|
| System Admin | Server configuration, user management |
| Security Admin | Encryption keys, audit policies |
| Audit Admin | View audit logs, compliance reports |

## Data Masking (Enterprise)

Column-level data masking for sensitive fields:

```sql
-- Create masked view
CREATE MASKED VIEW users_safe AS
  SELECT id, MASK(email) as email, MASK(phone) as phone, name
  FROM users
```

Masking types:
- `MASK` — Replace with `***`
- `HASH` — SHA-256 hash
- `TRUNCATE` — Keep first N characters
