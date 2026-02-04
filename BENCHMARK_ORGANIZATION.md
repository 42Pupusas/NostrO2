# Benchmark Organization

The benchmarks are now properly organized by scope:

## 📦 `nostro2/benches/` - Protocol-Level Benchmarks

Core Nostr protocol types and operations (no async/relay infrastructure).

### Benchmarks:
1. **`serialization.rs`** - JSON encode/decode for protocol messages
   - NostrClientEvent serialization/deserialization
   - NostrRelayEvent serialization/deserialization
   - Roundtrip performance
   - Message size impact

2. **`subscription.rs`** - Event filtering logic
   - Filter by author
   - Filter by kind
   - Filter by timestamp
   - Filter by IDs
   - Multi-filter combinations
   - Filter with limits

### Run:
```bash
cd nostro2
cargo bench                          # All protocol benchmarks
cargo bench --bench serialization    # Just serialization
cargo bench --bench subscription     # Just filtering
```

---

## ⚡ `nostro2-relay/benches/` - Relay Infrastructure Benchmarks

Relay-specific async infrastructure and performance characteristics.

### Benchmarks:
1. **`deduplication.rs`** - HashSet deduplication strategy
   - Sequential insertions
   - Lookup performance
   - Concurrent operations
   - Memory overhead

2. **`relay_benchmarks.rs`** - Message passing and channels
   - Unbounded/bounded MPSC throughput
   - Broadcast channels
   - Full message pipeline
   - Concurrent client simulation
   - Variable content sizes

### Run:
```bash
cd nostro2-relay
cargo bench                          # All relay benchmarks
cargo bench --bench deduplication    # Just deduplication
cargo bench --bench relay_benchmarks # Just relay ops
```

---

## 🎯 Why This Organization?

### Before:
- All benchmarks in `nostro2-relay/`
- Protocol benchmarks mixed with relay infrastructure
- Hard to benchmark protocol changes independently

### After:
- **Protocol benchmarks** → `nostro2/` (pure logic, no async)
- **Relay benchmarks** → `nostro2-relay/` (async infrastructure)
- Clear separation of concerns
- Can benchmark protocol improvements without relay overhead

---

## 📊 Quick Reference

| Benchmark | Location | What it Tests |
|-----------|----------|---------------|
| Serialization | `nostro2/` | JSON encode/decode |
| Subscription filtering | `nostro2/` | Event filtering logic |
| Deduplication | `nostro2-relay/` | SeenNotes HashSet |
| Channel throughput | `nostro2-relay/` | MPSC/broadcast channels |
| Message pipeline | `nostro2-relay/` | Full relay message flow |
| Concurrent clients | `nostro2-relay/` | Multi-client scenarios |

---

## 🚀 Running All Benchmarks

```bash
# From workspace root
cd nostro2 && cargo bench && cd ../nostro2-relay && cargo bench

# Or with a script (create this):
#!/bin/bash
echo "=== Protocol Benchmarks ==="
(cd nostro2 && cargo bench)
echo ""
echo "=== Relay Benchmarks ==="
(cd nostro2-relay && cargo bench)
```

---

## 📈 Baseline Comparison Workflow

### 1. Establish baselines
```bash
# Protocol baseline
cd nostro2
cargo bench -- --save-baseline protocol-v1

# Relay baseline
cd ../nostro2-relay
cargo bench -- --save-baseline relay-v1
```

### 2. Make changes
- Modify protocol types → affects `nostro2/benches/`
- Modify relay infrastructure → affects `nostro2-relay/benches/`

### 3. Compare
```bash
# After protocol changes
cd nostro2
cargo bench -- --baseline protocol-v1

# After relay changes
cd nostro2-relay
cargo bench -- --baseline relay-v1
```

---

## 📝 Documentation

- `nostro2/benches/README.md` - Protocol benchmark details
- `nostro2-relay/benches/README.md` - Relay benchmark details
- `nostro2-relay/BENCHMARKS.md` - Quick start guide
- This file - Organization overview

---

## ✅ What Changed

### Files Moved:
- `nostro2-relay/benches/serialization.rs` → `nostro2/benches/serialization.rs`

### Files Created:
- `nostro2/benches/subscription.rs` (extracted from relay_benchmarks.rs)
- `nostro2/benches/README.md`
- `nostro2/Cargo.toml` - Added criterion + bench harnesses

### Files Modified:
- `nostro2-relay/benches/relay_benchmarks.rs` - Removed subscription filtering
- `nostro2-relay/benches/README.md` - Updated docs
- `nostro2-relay/BENCHMARKS.md` - Updated docs
- `nostro2-relay/Cargo.toml` - Removed serialization harness

### Files Deleted:
- None (serialization.rs moved, not deleted)

---

## 🎓 Best Practices

1. **Protocol changes** → Run `nostro2` benchmarks first
2. **Relay changes** → Run `nostro2-relay` benchmarks
3. **Both changed** → Run both benchmark suites
4. **Always baseline** before making experimental changes
5. **Document findings** in commit messages or benchmark comments

---

## 🔮 Future Improvements

Consider adding:
- `nostro2/benches/validation.rs` - Signature verification, ID validation
- `nostro2/benches/tags.rs` - NostrTags operations
- `nostro2-relay/benches/pool.rs` - Multi-relay pool strategies
- `nostro2-relay/benches/websocket.rs` - WebSocket I/O benchmarks
