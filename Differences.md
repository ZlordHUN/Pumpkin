# Vanilla Parity Differences

## Overworld Fossils

Compared against the vanilla Minecraft 26.2 implementation and data from the
bundled server JAR.

### Confirmed Differences

#### High: Placement is not clipped to the originating chunk

Vanilla limits fossil block placement to the 16x16 chunk containing the
configured feature origin. Pumpkin currently permits placement from one chunk
before through one chunk after the origin chunk, creating a 3x3-chunk placement
area.

As a result, fossils near chunk boundaries contain bone and ore blocks that
vanilla deliberately omits. This is particularly noticeable for the
13-block-long spine templates.

Relevant code:
`pumpkin-world/src/generation/feature/features/fossil.rs:110`

When changing the placement box to the originating chunk, the integrity RNG
draw must also occur before the bounding-box check. Vanilla applies structure
processors, consuming their randomness, before it clips blocks to the chunk.
The current ordering would otherwise change which in-chunk blocks survive the
integrity processor.

Relevant code:
`pumpkin-world/src/generation/feature/features/fossil.rs:176`

#### Medium: Loaded neighboring chunks report an ocean-floor height of zero

Fossil placement scans the complete rotated template footprint for the lowest
`OCEAN_FLOOR_WG` height. When that footprint enters a persisted `Chunk::Level`
neighbor, Pumpkin currently returns a hard-coded height of zero.

This can place fossils generated beside existing chunks around Y -15 through
-24 instead of beneath the actual terrain. The limitation predates the fossil
implementation, but it prevents fossil parity.

Relevant code:
`pumpkin-world/src/chunk_system/generation_cache.rs:279`

### Confirmed Matching Behavior

- All 16 fossil templates match vanilla in dimensions, palettes, block
  positions, and states. Only the non-behavioral `DataVersion` value differs.
- The eight fossil and overlay template pairs and their selection order match.
- Fossils are wired to desert, swamp, and mangrove swamp biomes.
- The 1-in-64 rarity and upper and lower height ranges match.
- Rotation selection, template centering, burial depth, and empty-corner
  rejection match.
- Bone integrity is 90 percent and coal or diamond overlay integrity is 10
  percent.
- Protected-block filtering matches `#minecraft:features_cannot_replace`.
- Diamond fossils correctly replace overlay coal ore with deepslate diamond
  ore.
- Rotated block states, including bone-block axes, match.

### Test Coverage

`cargo test -p pumpkin-world fossil --lib` compiles successfully, but currently
runs no fossil-specific tests.

## Ancient Cities

Compared against the vanilla Minecraft 26.2 structure configuration, template
pools, templates, jigsaw implementation, sculk spreader, and terrain adaptation
behavior from the bundled server JAR.

### Confirmed Differences

#### High: Named start-jigsaw selection does not consume vanilla RNG

Vanilla obtains a named starting jigsaw by shuffling all jigsaws in the selected
start element, sorting them by selection priority, and then finding the requested
name. Pumpkin scans the unshuffled template order and immediately takes the
first matching jigsaw.

Each ancient-city center template contains five jigsaws and one
`minecraft:city_anchor`. Although both implementations find the same anchor,
Pumpkin omits the four Fisher-Yates random draws used by vanilla. Every later
jigsaw choice therefore starts from a different RNG state, producing a
different city graph for the same world seed.

Relevant code:
`pumpkin-world/src/generation/structure/structures/jigsaw_placement.rs:100`

#### High: Jigsaw collision handling does not support children inside a source piece

Vanilla maintains a separate free-space volume for connections whose target
position is inside the source piece. Pumpkin checks every candidate against all
piece bounding boxes, including its source piece, and has no equivalent
source-local free volume.

This rejects ancient-city sculk feature elements intended to be placed inside
rooms. The `ice_box_1`, `chamber_1`, `chamber_2`, `chamber_3`, `sauna_1`, and
`barracks` templates all contain upward-facing internal connectors targeting
the `minecraft:ancient_city/sculk` pool.

The same collision helper also treats inclusive `BlockBox` maxima as exclusive.
It can consequently allow two pieces to occupy the same one-block-thick layer,
which vanilla's deflated AABB check rejects.

Relevant code:
`pumpkin-world/src/generation/structure/structures/jigsaw_placement.rs:466`
`pumpkin-world/src/generation/structure/structures/jigsaw_placement.rs:610`

#### High: A 180-degree rotation reverses vertical jigsaw directions

Rotation around the Y axis must leave `Up` and `Down` unchanged. Pumpkin handles
180-degree rotation by calling `opposite()` for every direction, turning `Up`
into `Down` and `Down` into `Up`.

Ancient-city templates use `up_*` and `down_*` jigsaw orientations. Under a
180-degree rotation, their attachment direction and target position are
therefore wrong. This also affects internal sculk connectors.

Relevant code:
`pumpkin-world/src/generation/structure/structures/jigsaw_placement.rs:751`

#### High: Ancient-city sculk patches do not use vanilla spreading behavior

Vanilla uses its world-generation `SculkSpreader` state machine, including
charge splitting and merging, decay delays, growth costs and radius, sculk
block behavior callbacks, obstruction checks, and randomized non-corner
movement.

Pumpkin instead moves each simplified cursor by three independent offsets in
`-1..=1`, which also permits the zero vector, decreases charge by one, and
immediately places veins on all six neighboring faces. The generated sculk,
veins, catalysts, and subsequent rare-growth positions and RNG state do not
match vanilla.

The jigsaw-specific path additionally refuses to read or write outside the
current `ProtoChunk`, while vanilla sculk patches may cross a chunk boundary.

Relevant code:
`pumpkin-world/src/generation/feature/features/sculk_patch.rs:223`
`pumpkin-world/src/generation/feature/features/sculk_patch.rs:287`
`pumpkin-world/src/generation/feature/features/sculk_patch.rs:328`

#### Medium: Jigsaw maximum-distance bounds differ from vanilla

Vanilla decodes the ancient city's scalar `max_distance_from_center` value of
116 as both the horizontal and vertical limit. Pumpkin uses 116 horizontally
but hard-codes the vertical limit to 384.

Pumpkin also represents vanilla's exclusive upper AABB endpoint as an inclusive
`BlockBox` maximum while retaining the `+1`. This permits candidate pieces one
block beyond the configured positive X, Y, and Z limits. Together these
differences can accept elevated or edge pieces that vanilla rejects.

The upper world bound is also hard-coded as `min_y + 320`, which is Y 256 in
the Overworld, instead of using the dimension's exclusive maximum Y of 320.
This does not reproduce vanilla's clamping behavior.

Relevant code:
`pumpkin-world/src/generation/structure/structures/jigsaw_placement.rs:42`
`pumpkin-world/src/generation/structure/structures/jigsaw_placement.rs:154`

#### Low: Beard-box terrain density is not numerically identical

Vanilla computes beard contributions with `Mth.fastInvSqrt`, which uses its
specific floating-point approximation. Pumpkin uses an exact square root and
reciprocal while describing it as equivalent. The resulting density values are
not identical and can change terrain blocks near a zero-density boundary around
the city.

Relevant code:
`pumpkin-world/src/generation/noise/router/density_function/beardifier.rs:52`

### Confirmed Matching Behavior

- The structure-set spacing of 24, separation of 8, salt of 20083232, and
  single structure weight match.
- The structure is restricted to the deep-dark biome and uses the underground
  decoration step.
- Start height -27, `minecraft:city_anchor`, depth 7, beard-box terrain
  adaptation, and horizontal distance 116 are wired from vanilla data.
- All seven template pools match vanilla element order, weights, projections,
  processors, and fallbacks.
- All 58 structure templates match vanilla dimensions, palettes, block
  positions, states, and NBT. Only the non-behavioral `DataVersion` differs.
- The generic, start, and wall degradation processor lists match vanilla data.
- Protected-block filtering and degradation rule probabilities match.
- The current branch uses the vanilla ground-level delta of one for jigsaw pool
  elements; this corrects the zero used by the originally merged implementation.

### Test Coverage

`cargo test -p pumpkin-world ancient_city --lib` passes all six focused tests.
Those tests verify registration, pool weights, embedded templates, anchor
presence, generation of a multi-piece graph, and that a reference chunk
contains some city blocks without leftover jigsaws. They do not compare exact
piece selection, positions, rotations, sculk output, terrain adaptation, or
the final block map against vanilla.
