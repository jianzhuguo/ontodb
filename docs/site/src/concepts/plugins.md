# Plugin System

OntoDB supports plugins that extend the INSERT/UPDATE/DELETE pipeline.

## Architecture

```
Client Request
      ↓
┌─────────────┐
│  Plugin Hook │ ← Pre-operation
│  (before)    │
└──────┬──────┘
       ↓
┌─────────────┐
│  Operation   │ ← INSERT / UPDATE / DELETE
└──────┬──────┘
       ↓
┌─────────────┐
│  Plugin Hook │ ← Post-operation
│  (after)     │
└──────┬──────┘
       ↓
  Response
```

## Plugin Interface

Plugins implement hooks that run before and after data operations.

### Hook Types

| Hook | Trigger | Use Case |
|------|---------|----------|
| `pre_insert` | Before INSERT | Validation, transformation, enrichment |
| `post_insert` | After INSERT | Indexing, notification, audit |
| `pre_update` | Before UPDATE | Validation, authorization |
| `post_update` | After UPDATE | Cache invalidation, audit |
| `pre_delete` | Before DELETE | Authorization, cascade |
| `post_delete` | After DELETE | Cleanup, notification |

### Plugin Context

Each hook receives a context object:

```rust
struct PluginContext {
    // Operation type
    operation: Operation,  // INSERT, UPDATE, DELETE
    
    // Target class
    class: String,
    
    // Document data (for INSERT/UPDATE)
    data: Option<serde_json::Value>,
    
    // Primary key (for UPDATE/DELETE)
    key: Option<String>,
    
    // Metadata
    metadata: HashMap<String, String>,
}
```

## Built-in Plugins

### Audit Plugin

Logs all data modifications:

```rust
struct AuditPlugin;

impl Plugin for AuditPlugin {
    fn post_insert(&self, ctx: &PluginContext) -> Result<()> {
        log::info!("INSERT into {}: {:?}", ctx.class, ctx.data);
        Ok(())
    }
    
    fn post_update(&self, ctx: &PluginContext) -> Result<()> {
        log::info!("UPDATE {}: key={:?}", ctx.class, ctx.key);
        Ok(())
    }
    
    fn post_delete(&self, ctx: &PluginContext) -> Result<()> {
        log::info!("DELETE from {}: key={:?}", ctx.class, ctx.key);
        Ok(())
    }
}
```

### Validation Plugin

Validates data before insertion:

```rust
struct ValidationPlugin;

impl Plugin for ValidationPlugin {
    fn pre_insert(&self, ctx: &PluginContext) -> Result<()> {
        if let Some(data) = &ctx.data {
            // Validate required fields
            if data["email"].as_str().is_none() {
                return Err(Error::Validation("email is required"));
            }
        }
        Ok(())
    }
}
```

### Enrichment Plugin

Adds metadata to documents:

```rust
struct EnrichmentPlugin;

impl Plugin for EnrichmentPlugin {
    fn pre_insert(&self, ctx: &mut PluginContext) -> Result<()> {
        if let Some(data) = &mut ctx.data {
            data["created_at"] = json!(chrono::Utc::now().timestamp());
            data["version"] = json!(1);
        }
        Ok(())
    }
}
```

## Plugin Registry

Plugins are registered at startup:

```rust
let mut registry = PluginRegistry::new();

// Register plugins
registry.register(Box::new(AuditPlugin));
registry.register(Box::new(ValidationPlugin));
registry.register(Box::new(EnrichmentPlugin));

// Plugins execute in registration order
```

## Plugin Execution

Hooks execute in registration order:

1. Pre-hooks run in order (first registered → last)
2. Operation executes
3. Post-hooks run in order (first registered → last)

If any pre-hook returns an error, the operation is aborted.

## Use Cases

| Use Case | Plugin Type | Description |
|----------|-------------|-------------|
| Audit trail | Post-hook | Log all modifications |
| Data validation | Pre-hook | Validate before write |
| Auto-enrichment | Pre-hook | Add timestamps, defaults |
| Cache invalidation | Post-hook | Invalidate cache on write |
| Event sourcing | Post-hook | Publish events to message queue |
| Access control | Pre-hook | Check permissions before write |
| Data transformation | Pre-hook | Transform data format |
| Indexing | Post-hook | Update external indexes |
