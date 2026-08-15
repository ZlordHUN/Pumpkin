# Memory Leak Audit

**Audit date:** 2026-08-15<br>
**Audited base:** `upstream/master` at `c13c5150`<br>
**Related pull request:** [#2907 — release chunk holders, dependency edges and tickets during travel](https://github.com/Pumpkin-MC/Pumpkin/pull/2907) (`b43befb8`)<br>
**Status:** Static ownership/lifecycle audit completed; targeted chunk-system tests pass.

## Executive summary

PR #2907 fixes important causes of unbounded chunk-system growth, but it does not fix every memory-retention path reached during exploration, disconnect, dimension changes, or entity unloading.

The audit found:

- three logical chunk-system leaks addressed by PR #2907;
- six additional definite ownership or cache leaks;
- six cancellation, concurrency, backpressure, or future world-unload paths capable of unbounded retention; and
- allocator RSS retention, which is observable memory growth but is distinct from live-object leaks.

The highest-priority work after applying the logical changes from PR #2907 is:

1. break the two independent player ownership cycles;
2. evict clean region serializers and bound the Bedrock blob cache;
3. replace strong mob-goal and vehicle backlinks with weak references;
4. make temporary chunk tickets cancellation-safe;
5. serialize watch transitions and bound chunk-send work; and
6. add lifetime regression tests and memory/cardinality metrics.

## Scope and validation

The audit followed strong `Arc` ownership, scheduler tickets and dependency edges, region-file caches, listener registrations, detached tasks, entity relationships, and disconnect/world-transition cleanup paths.

The following targeted suite was run on the audited base:

```text
cargo test -p pumpkin-world --lib chunk_system
14 passed; 0 failed; 134 filtered out
```

These tests validate existing chunk-system behavior, but they do not assert that chunks, holders, players, serializers, entities, or tasks are eventually dropped. Each finding below therefore includes a proposed lifetime-specific regression test.

## Finding classification

- **Definite:** a strong ownership cycle or map/cache without a complete removal path. The retained object can be proven from the code's ownership graph.
- **Conditional:** unbounded retention requires cancellation, backpressure, a particular ordering, or API misuse.
- **Latent:** the cycle exists now, but a currently long-lived owner hides it until a future unload/reload feature attempts to free the object.

## PR #2907 findings

### PR-01: Chunk-holder dependency stage is a high-water mark

**Severity:** Critical<br>
**Status:** Addressed by PR #2907

`ChunkHolder::dependency_stage` can increase but is not recomputed downward when dependents disappear. Consequently, a holder's effective target may never return to `None`; holders, completed tasks, and their chunks remain reachable.

Relevant code:

- [`crates/pumpkin-world/src/chunk_system/schedule.rs`](crates/pumpkin-world/src/chunk_system/schedule.rs)

**Required change:** Apply the PR's dependency-stage recomputation and make dependency garbage collection run independently of whether the unload queue happens to be non-empty. Purge tasks that can no longer contribute to a live target.

**Regression test:** Construct a multi-stage dependency chain, remove all external tickets, run dependency collection, and assert that holder count, graph node/edge count, and queued task count all return to zero.

### PR-02: `occupied_by` dependency edges survive unload

**Severity:** Critical<br>
**Status:** Addressed by PR #2907

Unloading a holder does not release the full `occupied_by` chain. During continuous travel, these edges accumulate much faster than the visible loaded-chunk count.

Relevant cleanup path:

- [`schedule.rs:591`](crates/pumpkin-world/src/chunk_system/schedule.rs#L591)
- [`schedule.rs:645`](crates/pumpkin-world/src/chunk_system/schedule.rs#L645)

**Required change:** Apply the PR's explicit edge-chain release and verify that both outgoing and incoming dependency state are cleared before the holder becomes unreachable.

**Regression test:** Repeatedly generate and unload adjacent dependency chains, then assert that the graph has no edges or occupied holders after collection.

### PR-03: Simulation tickets survive disconnect and dimension change

**Severity:** Critical<br>
**Status:** Addressed by PR #2907

`ChunkManager` cleanup removes view tickets but loses track of simulation tickets. Disconnects and world changes can therefore retain the simulation-distance area for each incident. View-ticket recalculation can also use the wrong retained value.

Relevant code:

- [`crates/pumpkin/src/entity/player.rs:367`](crates/pumpkin/src/entity/player.rs#L367)
- [`crates/pumpkin/src/entity/player.rs:452`](crates/pumpkin/src/entity/player.rs#L452)
- [`crates/pumpkin/src/entity/player.rs:526`](crates/pumpkin/src/entity/player.rs#L526)
- [`crates/pumpkin/src/entity/player.rs:548`](crates/pumpkin/src/entity/player.rs#L548)

**Required change:** Store and update view and simulation tickets as a pair, and remove both from the old level on disconnect or dimension transfer.

**Regression test:** Join, move, change dimension, disconnect, run the scheduler to quiescence, and assert that no ticket owned by that player remains in either level.

### PR-04: Allocator RSS retention is not a live-object leak

**Severity:** Operational<br>
**Status:** Mitigated by PR #2907's mimalloc change

After logical objects are freed, the system allocator may retain arenas and buffers, leaving RSS high even when live heap usage is flat. Switching allocators can improve returned-memory behavior, but it must not be used as evidence that the ownership leaks are fixed.

**Required change:** Keep allocator selection as a separately benchmarked change. Measure both live allocated bytes/object counts and process RSS. Accept the logical fix only when live scheduler/cache/entity counts return to a bounded baseline.

## Additional definite leaks

### ML-01: `Player` and `SyncHandler` form a permanent strong cycle

**Severity:** Critical<br>
**Classification:** Definite<br>
**Growth unit:** One complete player object graph per session

`Player` owns `screen_handler_sync_handler: Arc<SyncHandler>`. `SyncHandler` stores `Option<Arc<dyn InventoryPlayer>>`, and server setup stores the same player in the handler. There is no matching clear operation.

Ownership path:

```text
Player -> Arc<SyncHandler> -> Arc<dyn InventoryPlayer> -> Player
```

Evidence:

- [`player.rs:745`](crates/pumpkin/src/entity/player.rs#L745)
- [`sync_handler.rs:45`](crates/pumpkin-inventory/src/sync_handler.rs#L45)
- [`sync_handler.rs:67`](crates/pumpkin-inventory/src/sync_handler.rs#L67)
- [`server/mod.rs:579`](crates/pumpkin/src/server/mod.rs#L579)

**Required change:** Store a `Weak<dyn InventoryPlayer>` in `SyncHandler`, upgrading it only for individual operations. As defense in depth, add an explicit detach operation to player cleanup.

**Regression test:** Keep a `Weak<Player>`, perform normal connection setup and cleanup, drop all external strong references, and assert that `Weak::upgrade()` returns `None`.

### ML-02: `Player` and the Java/Bedrock client form another strong cycle

**Severity:** Critical<br>
**Classification:** Definite<br>
**Growth unit:** One player, client, queues, caches, and associated state per session

`Player` owns an `Arc<ClientPlatform>`. Java and Bedrock client state stores a strong `Arc<Player>` after login. Connection close and listener shutdown do not clear the player slot.

Ownership paths:

```text
Player -> ClientPlatform::Java -> Arc<Player> -> Player
Player -> ClientPlatform::Bedrock -> Arc<BedrockClient> -> Arc<Player> -> Player
```

Evidence:

- [`net/mod.rs:117`](crates/pumpkin/src/net/mod.rs#L117)
- [`net/java/mod.rs:87`](crates/pumpkin/src/net/java/mod.rs#L87)
- [`net/java/mod.rs:178`](crates/pumpkin/src/net/java/mod.rs#L178)
- [`net/bedrock/mod.rs:95`](crates/pumpkin/src/net/bedrock/mod.rs#L95)
- [`net/bedrock/mod.rs:382`](crates/pumpkin/src/net/bedrock/mod.rs#L382)
- [`lib.rs:545`](crates/pumpkin/src/lib.rs#L545)
- [`lib.rs:649`](crates/pumpkin/src/lib.rs#L649)

**Required change:** Store a weak player backlink in both client implementations. If changing the representation is invasive, clear the client-side player reference before removal from the server player list; the weak representation is still preferable because it makes accidental cycles impossible.

**Regression test:** Cover Java and Bedrock login/disconnect independently. After listener completion and task-tracker shutdown, assert that weak player and client handles can no longer be upgraded.

### ML-03: Clean chunk-region serializers are never evicted

**Severity:** High<br>
**Classification:** Definite<br>
**Growth unit:** One serializer and retained region data per visited region file

`ChunkFileManager.file_locks` is a `BTreeMap<PathBuf, Arc<ChunkSerializerLazyLoader<_>>>`. `get_serializer` inserts serializers into the map. `maybe_evict` is only called from the save path. A clean `Chunk::Level` is dropped during scheduler unload without being sent through `io_write`, so regions containing only clean loaded chunks never reach the only eviction call.

An Anvil serializer can retain metadata and compressed `Bytes` for up to 1,024 chunks in the region. Continuous travel across an existing or pre-generated world therefore grows the serializer map even if the visible chunk map drains correctly.

Evidence:

- [`file_manager.rs:45`](crates/pumpkin-world/src/chunk/io/file_manager.rs#L45)
- [`file_manager.rs:55`](crates/pumpkin-world/src/chunk/io/file_manager.rs#L55)
- [`file_manager.rs:129`](crates/pumpkin-world/src/chunk/io/file_manager.rs#L129)
- [`file_manager.rs:163`](crates/pumpkin-world/src/chunk/io/file_manager.rs#L163)
- [`file_manager.rs:392`](crates/pumpkin-world/src/chunk/io/file_manager.rs#L392)
- [`schedule.rs:679`](crates/pumpkin-world/src/chunk_system/schedule.rs#L679)
- [`anvil.rs:72`](crates/pumpkin-world/src/chunk/format/anvil.rs#L72)

**Required change:** Give serializer entries an explicit lifecycle independent of writes. Suitable designs include:

- evicting after a read when no watcher/operation lease remains;
- removing the entry when the last chunk in the region becomes unwatched; or
- using a size- and entry-bounded LRU with active-operation pinning.

The map must not remove a serializer while an I/O operation still uses it.

**Regression test:** Traverse thousands of clean, pre-generated chunks, unload them, wait for I/O completion, and assert that serializer entry count and retained compressed bytes return to a small bounded working set.

### ML-04: Bedrock's per-client blob cache has no eviction path

**Severity:** High<br>
**Classification:** Definite<br>
**Growth unit:** Encoded subchunk blobs for every distinct chunk sent by a long-lived Bedrock session

`BedrockClient.blob_cache` is an unbounded `HashMap<u64, Vec<u8>>`. Chunk sends insert new blobs. Cache-status handling clones requested entries but does not remove acknowledged, obsolete, or least-recently-used blobs.

Evidence:

- [`net/bedrock/mod.rs:120`](crates/pumpkin/src/net/bedrock/mod.rs#L120)
- [`net/bedrock/mod.rs:353`](crates/pumpkin/src/net/bedrock/mod.rs#L353)
- [`net/bedrock/mod.rs:794`](crates/pumpkin/src/net/bedrock/mod.rs#L794)

The player/client cycles in ML-01 and ML-02 amplify this leak by preventing the client cache from being freed after disconnect.

**Required change:** Define a protocol-correct retention window, remove blobs once they no longer need retransmission, and enforce a hard entry/byte budget with LRU eviction. Record total cached bytes, not only entry count.

**Regression test:** Send a large sequence of unique chunks to one Bedrock client while processing cache-status packets. Assert that cache bytes never exceed the configured limit and that client cleanup drops the entire cache.

### ML-05: Several mob goals retain their owning mob strongly

**Severity:** High<br>
**Classification:** Definite<br>
**Growth unit:** One mob and its entity/AI state per affected unloaded mob

`MobEntity` owns a `GoalSelector`, which owns its goals. Most goal implementations use weak mob references, but several affected goals store a strong `Arc` to the same mob:

- Creeper ignite goal: [`creeper.rs:75`](crates/pumpkin/src/entity/mob/creeper.rs#L75), [`creeper_ignite.rs:9`](crates/pumpkin/src/entity/ai/goal/creeper_ignite.rs#L9)
- Enderman goals: [`enderman.rs:105`](crates/pumpkin/src/entity/mob/enderman.rs#L105)
- Slime goals: [`slime.rs:73`](crates/pumpkin/src/entity/mob/slime.rs#L73)
- Shulker goals: [`shulker.rs:99`](crates/pumpkin/src/entity/mob/shulker.rs#L99)

Ownership path:

```text
Mob -> GoalSelector -> Goal -> Arc<Mob> -> Mob
```

`clear_ai_goals` exists, but normal entity/chunk removal does not call it.

**Required change:** Convert owner references in all goal implementations to `Weak`. Audit new goals through a common helper or API that makes strong self-ownership difficult. Clear goals during removal as defense in depth.

**Regression test:** Spawn and unload at least one Creeper, Enderman, Slime, and Shulker. Retain only `Weak` handles and assert that all upgrades fail after entity cleanup.

### ML-06: Vehicle and passenger references form two-way strong cycles

**Severity:** High<br>
**Classification:** Definite<br>
**Growth unit:** Each mounted entity group unloaded without an explicit dismount

An entity strongly owns its passengers and a passenger strongly owns its vehicle. World chunk cleanup removes and saves entities but does not sever vehicle, passenger, or leash relationships.

Evidence:

- [`entity/mod.rs:845`](crates/pumpkin/src/entity/mod.rs#L845)
- [`entity/mod.rs:3298`](crates/pumpkin/src/entity/mod.rs#L3298)
- [`entity/mod.rs:3343`](crates/pumpkin/src/entity/mod.rs#L3343)
- [`world/mod.rs:4586`](crates/pumpkin/src/world/mod.rs#L4586)

**Required change:** Make the passenger-to-vehicle backlink weak, or introduce mandatory relationship teardown before any entity leaves world ownership. Also detach leash relationships during the same cleanup transaction.

**Regression test:** Unload a mounted entity pair and a multi-passenger vehicle without manually dismounting them. Assert that every weak entity handle expires and reloading does not duplicate the relationship.

## Conditional and latent retention paths

### ML-07: Ad-hoc fetch tickets are not cancellation-safe

**Severity:** High<br>
**Classification:** Conditional on future cancellation or panic

`Level::fetch_chunk` installs a listener, adds a temporary level-31 ticket, awaits the receiver, and only then removes the ticket. Dropping the future between ticket creation and normal completion strands the ticket and can retain the target chunk and dependency chain indefinitely.

Evidence:

- [`level.rs:650`](crates/pumpkin-world/src/level.rs#L650)
- [`world/mod.rs:294`](crates/pumpkin/src/world/mod.rs#L294)

**Required change:** Return or create an RAII ticket guard at registration time. Its `Drop` implementation must enqueue a non-blocking removal command even when the future is cancelled. Listener registration should use the same cancellation guard.

**Regression test:** Poll `fetch_chunk` until its ticket is visible, cancel the future, run the scheduler, and assert that the ticket, listener, holder, and dependency chain disappear.

### ML-08: Detached chunk-send tasks can grow without a producer bound

**Severity:** High under backpressure<br>
**Classification:** Conditional on a slow network writer or plugin/event processing

Player ticking pops strong `Arc<ChunkData>` batches and starts detached `tokio::spawn` work. These tasks are not owned by the client's task tracker. Packet channels are bounded, but the number of detached producers waiting to use them is not, so stalled consumers can leave an increasing number of tasks retaining chunk data and encoded buffers.

Evidence:

- [`player.rs:2323`](crates/pumpkin/src/entity/player.rs#L2323)
- [`net/java/mod.rs:283`](crates/pumpkin/src/net/java/mod.rs#L283)
- [`net/java/mod.rs:475`](crates/pumpkin/src/net/java/mod.rs#L475)
- [`net/bedrock/mod.rs:294`](crates/pumpkin/src/net/bedrock/mod.rs#L294)

**Required change:** Use one owned per-client send pipeline with a bounded work queue or semaphore. Track it in the client's `TaskTracker`, connect it to the cancellation token, and prevent multiple overlapping batch encoders from exceeding a fixed byte/job budget.

**Regression test:** Stall the outgoing writer while moving a player continuously. Assert a fixed upper bound on live send jobs and retained chunk/packet bytes, then disconnect and assert all jobs terminate.

### ML-09: Concurrent watch transitions can leave stale watcher counts

**Severity:** High<br>
**Classification:** Conditional race

`mark_chunks_as_newly_watched` and `mark_chunks_as_not_watched` update chunk state and then await entity-saver watcher operations. `chunker::update_position` stores the new watched section before these awaits finish. Player position updates can originate from world ticks and network handlers concurrently, and disconnect cleanup is another independent path.

An unwatch can therefore reach the entity saver before an older watch operation. If the older watch finishes last, a stale file watcher/refcount remains and prevents entity-region serializer eviction. Overlapping position updates can also duplicate ticket or watcher transitions.

Evidence:

- [`level.rs:442`](crates/pumpkin-world/src/level.rs#L442)
- [`level.rs:457`](crates/pumpkin-world/src/level.rs#L457)
- [`chunker.rs:98`](crates/pumpkin/src/world/chunker.rs#L98)
- [`player.rs:1185`](crates/pumpkin/src/entity/player.rs#L1185)

**Required change:** Serialize each player's complete watch transition with one async mutex/state machine. Use generation numbers if work may complete out of order, and make watch/ticket changes idempotent for a `(player, chunk)` key.

**Regression test:** Deliberately delay watch and unwatch operations while issuing overlapping moves and disconnect. Assert zero player tickets and zero entity-region watchers after cleanup.

### ML-10: Global chunk-listener registrations are only opportunistically pruned

**Severity:** Medium<br>
**Classification:** Conditional, deferred cleanup

Every player creates a global listener channel. Disconnect drops or replaces the receiver, but the sender remains in `ChunkListener.global`. Dead senders are removed only when another chunk completion attempts to send through them. Repeated connect/disconnect cycles during a period with no new chunk completions therefore grow the vector.

Single-chunk listeners have a related cancellation problem: they are removed only when their matching chunk completes.

Evidence:

- [`chunk_listener.rs:9`](crates/pumpkin-world/src/chunk_system/chunk_listener.rs#L9)
- [`chunk_listener.rs:29`](crates/pumpkin-world/src/chunk_system/chunk_listener.rs#L29)
- [`chunk_listener.rs:67`](crates/pumpkin-world/src/chunk_system/chunk_listener.rs#L67)
- [`player.rs:989`](crates/pumpkin/src/entity/player.rs#L989)
- [`player.rs:536`](crates/pumpkin/src/entity/player.rs#L536)

**Required change:** Return a registration ID/guard from listener creation and deregister it in `Drop` or explicit player cleanup. Do not depend on future chunk traffic to remove dead registrations.

**Regression test:** Reconnect many clients without completing another chunk and assert that listener registration count returns to its baseline after every disconnect.

### ML-11: Cancelled scheduler task IDs can be retained forever

**Severity:** Low<br>
**Classification:** Conditional on cancelling a completed or unknown ID

`TaskScheduler.cancelled_tasks` accepts any ID. An entry is removed only when a matching queued task reaches its due time. Cancelling an already-completed or invalid ID creates a permanent set entry. Repeating task handlers are also spawned without overlap control, allowing a backlog when a handler takes longer than its interval.

Evidence:

- [`server/scheduler.rs:40`](crates/pumpkin/src/server/scheduler.rs#L40)
- [`server/scheduler.rs:101`](crates/pumpkin/src/server/scheduler.rs#L101)
- [`server/scheduler.rs:123`](crates/pumpkin/src/server/scheduler.rs#L123)
- [`server/scheduler.rs:139`](crates/pumpkin/src/server/scheduler.rs#L139)

**Required change:** Track active task IDs and ignore cancellation for IDs that are not active, or remove the task directly from scheduler ownership. Run repeating handlers through an owned `JoinSet`/`TaskTracker` with a defined no-overlap or bounded-overlap policy.

**Regression test:** Repeatedly cancel completed and random task IDs and assert that cancellation state remains empty. Run a slow repeating handler and assert that live invocations stay within the configured limit.

### ML-12: `World`, `Level`, and `WorldPortal` form a latent strong cycle

**Severity:** Medium for dynamic world reload/unload<br>
**Classification:** Latent

`World` strongly owns its `Level`. `Level.world_portal` strongly owns a `WorldPortal`, and `WorldPortal` strongly owns the original `World`. Even after removal from the server's world map, this cycle would prevent the world and its complete level state from being freed.

Ownership path:

```text
World -> Arc<Level> -> Arc<WorldPortal> -> Arc<World> -> World
```

Evidence:

- [`world/mod.rs:242`](crates/pumpkin/src/world/mod.rs#L242)
- [`level.rs:73`](crates/pumpkin-world/src/level.rs#L73)
- [`server/mod.rs:351`](crates/pumpkin/src/server/mod.rs#L351)
- [`server/mod.rs:471`](crates/pumpkin/src/server/mod.rs#L471)
- [`world/mod.rs:6201`](crates/pumpkin/src/world/mod.rs#L6201)

**Required change:** Make `WorldPortal` hold a weak world reference, or clear `Level.world_portal` as the first step of world removal. A weak backlink is the safer invariant.

**Regression test:** Create and register a world, remove/unload it, drop all external handles, and assert that weak `World` and `Level` handles expire.

## Paths audited that already have bounded or cleanup behavior

The following were inspected and should not be confused with the leaks above:

- stale `chunks_with_scheduled_ticks` entries are removed when their loaded chunk is absent;
- entity chunks and related maps have cleanup/shrink paths;
- `ChunkManager.chunk_sent` stores weak chunk references and is bounded by view cleanup; and
- Java and Bedrock outgoing packet channels are bounded. The ML-08 problem is the unbounded number of detached producers waiting outside those bounded channels.

## Recommended implementation order

### Phase 1: Stop the largest retained object graphs

1. Apply the logical scheduler and paired-ticket fixes from PR #2907.
2. Fix ML-01 and ML-02 together and add Java/Bedrock disconnect drop tests.
3. Fix ML-03 and expose serializer entry/byte metrics.
4. Bound and evict the Bedrock cache in ML-04.

### Phase 2: Make entity unloading complete

1. Convert all self-owning mob goals in ML-05 to weak references.
2. Break vehicle/passenger/leash relationships for ML-06.
3. Add entity drop tests to chunk-unload coverage.

### Phase 3: Make asynchronous cleanup cancellation-safe

1. Introduce RAII ticket/listener registrations for ML-07 and ML-10.
2. Serialize watch state transitions for ML-09.
3. Replace detached chunk sends with the bounded, client-owned pipeline in ML-08.
4. Harden general task cancellation and repeating work for ML-11.

### Phase 4: Prepare for full world unloading

1. Break the ML-12 portal backlink.
2. Add an integration test that repeatedly creates, loads, unloads, and drops a world.

## Required observability

Expose the following debug metrics so future regressions can be detected without relying on RSS alone:

- chunk holders, loaded chunks, pending unloads, queued generation tasks;
- dependency graph nodes, edges, and occupied edges;
- tickets by kind and owner;
- chunk and entity region serializer counts and retained compressed bytes;
- active players/clients and completed session cleanup count;
- Bedrock blob-cache entries and bytes per client;
- entity counts by type, including unloaded-but-live debug counters;
- chunk listener registrations;
- live chunk-send jobs and their retained bytes;
- active ad-hoc fetch tickets; and
- live/queued/cancelled general scheduler tasks.

For a travel/disconnect benchmark, all logical counts should return to a stable bounded baseline after the workload stops and cleanup reaches quiescence. RSS may remain above baseline depending on allocator policy, so live-object/cardinality metrics are the acceptance criterion for correctness.

## Definition of done

The memory-leak work is complete when:

1. the PR #2907 scheduler/ticket benchmark drains holders, edges, tasks, and tickets;
2. all lifetime tests proposed above pass for Java and Bedrock;
3. clean-region traversal and Bedrock traversal remain within explicit cache budgets;
4. affected mobs and mounted entities are demonstrably dropped after unload;
5. cancelled fetches and overlapping watch transitions leave no registrations or tickets;
6. slow-client testing cannot create unbounded chunk-send jobs; and
7. a counting allocator or equivalent live-heap instrumentation remains flat across repeated travel, disconnect, dimension-change, and unload cycles.
