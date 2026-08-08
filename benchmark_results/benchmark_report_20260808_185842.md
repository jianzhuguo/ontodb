# OntoDB Performance Benchmark Report

**Generated:** 2026-08-08 18:58:42
**Results Directory:** E:\ontodb\benchmark_results

## Summary

| Benchmark | Status | Duration | Key Metrics |
|-----------|--------|----------|-------------|
| onto-storage_lock_contention | ✅ PASS | 143724ms | write_throughput_50000: 1,017,555 ops/sec; read_throughput_50000: 1,352,097 ops/sec |
| onto-storage_vector_recall | ✅ PASS | 260500ms | N/A |
| onto-raft_raft_bench | ✅ PASS | 1324ms | log_append_1000: 586,270 ops/sec; log_append_10000: 605,188 ops/sec; log_append_50000: 617,700 ops/sec |
| onto-ontology_ontology_bench | ✅ PASS | 313940ms | small_(4_classes,_4_props): 0.0 ms; big_(100_classes,_200_props): 0.4 ms; huge_(500_classes,_1000_props): 1.7 ms |
| onto-graph_graph_bench | ❌ FAIL | 133245ms | N/A |
| onto-graph_graph_vector_recall | ✅ PASS | 10640ms | N/A |

## Detailed Results

### onto-storage_lock_contention

**Metrics:**

- write_throughput_50000: 1,017,555 ops/sec
- read_throughput_50000: 1,352,097 ops/sec

**Duration:** 143724ms

---

### onto-storage_vector_recall

**Metrics:**


**Duration:** 260500ms

---

### onto-raft_raft_bench

**Metrics:**

- log_append_1000: 586,270 ops/sec
- log_append_10000: 605,188 ops/sec
- log_append_50000: 617,700 ops/sec
- log_read_1000: 661,901 ops/sec
- log_read_10000: 626,617 ops/sec
- log_read_50000: 566,455 ops/sec
- state_machine_apply_1000: 993,937 ops/sec
- state_machine_apply_10000: 1,182,271 ops/sec
- state_machine_apply_50000: 1,112,053 ops/sec
- log_read_10000: 490,280 ops/sec
- log_read_10000: 537,461 ops/sec
- log_read_10000: 523,125 ops/sec
- batch_apply_100: 948,192 ops/sec
- snapshot_build_1000: 0.00 ms
- snapshot_build_10000: 0.00 ms
- snapshot_build_50000: 0.00 ms

**Duration:** 1324ms

---

### onto-ontology_ontology_bench

**Metrics:**

- small_(4_classes,_4_props): 0.01 ms
- big_(100_classes,_200_props): 0.36 ms
- huge_(500_classes,_1000_props): 1.74 ms
- mega_(2000_classes,_5000_props): 9.00 ms
- get_all_subclasses_(depth=3,_40_classes,_39_subs): 0.02 ms
- get_all_subclasses_(depth=5,_364_classes,_363_subs): 0.13 ms
- get_all_subclasses_(depth=7,_3280_classes,_3279_subs): 1.38 ms
- get_class_properties_(depth=7): 0.00 ms
- get_all_subclasses_(depth=10,_2047_classes,_2046_subs): 0.97 ms
- get_class_properties_(depth=10): 0.00 ms
- get_all_subclasses_(10_diamonds×depth5,_111_classes,_110_subs): 0.03 ms
- get_all_subclasses_(50_diamonds×depth5,_551_classes,_550_subs): 0.17 ms
- get_all_subclasses_(20_diamonds×depth10,_421_classes,_420_subs): 0.12 ms
- get_all_subclasses_(mesh_5×20×fan3,_101_classes,_100_subs): 0.06 ms
- get_all_subclasses_(mesh_8×30×fan5,_241_classes,_240_subs): 0.15 ms
- get_all_subclasses_(mesh_10×50×fan5,_501_classes,_500_subs): 0.31 ms

**Duration:** 313940ms

---

### onto-graph_graph_bench

**Error:** Exit code 3221225477

---

### onto-graph_graph_vector_recall

**Metrics:**


**Duration:** 10640ms

---

