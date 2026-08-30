# Living Data

OntoDB treats data as **alive** — every piece of data has a value score that decays over time, just like human memory.

## Value Decay

Every entity has a `value_score` that decays exponentially:

```
score(t) = value_score × e^(-λ × Δt)
```

Where:
- `value_score` — initial value (0.0 to 1.0)
- `λ` — decay rate (determines half-life)
- `Δt` — time elapsed since last access

### Half-Life Presets

| Preset | Half-Life | Use Case |
|--------|-----------|----------|
| Fast | 7 hours | Session data, temporary cache |
| Medium | 70 days | Business data, user activity |
| Slow | 2 years | Archival data, compliance records |

## Activation

You can "activate" an entity to boost its value score and reset the decay clock:

```sql
-- Activate a specific entity
SYSTEM ACTIVATE Product::"prod_1"

-- Activate all entities matching a condition
SYSTEM ACTIVATE Product WHERE category = "hot"
```

Activation is useful for:
- Keeping important data "alive" during analysis
- Temporarily boosting data that's being actively used
- Preventing decay of critical reference data

## Value Scoring

OntoDB can automatically score new documents based on:

| Rule | Weight | Criteria |
|------|--------|----------|
| Text length | 0.3 | Longer documents score higher |
| Completeness | 0.25 | More filled fields score higher |
| Structure | 0.25 | Well-structured data scores higher |
| Size | 0.2 | Optimal size range scores highest |

## Querying by Value

```sql
-- Get high-value data
SELECT * FROM Product WHERE value_score > 0.8

-- Get data temperature
SELECT * FROM system.data_temperature

-- Get value events (decay/activation history)
SELECT * FROM system.value_events
```

## Use Cases

| Scenario | Benefit |
|----------|---------|
| Cache management | Low-value data auto-evicts |
| Search ranking | High-value results rank higher |
| Data lifecycle | Old data naturally fades |
| AI memory | Mimics human forgetting curve |
