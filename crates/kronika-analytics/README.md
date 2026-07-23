# kronika-analytics

[Русская версия](README.ru.md)

`kronika-analytics` contains source-independent counter differences, anomaly
scoring, and the contract core for a future timeline overview.

## Overview contract core

`overview` defines retained event observations, exact event counts, coverage,
counter and gauge reductions, health evaluation, and an adapter interface for
semantic comparisons. It does not read PGM files, persist an overview index,
serve HTTP, or define response redaction.

Observation identities distinguish two cases:

- a sealed or externally proven locator produces a rebuild-stable
  content-derived lineage from source scope, naming contract, segment locator,
  and the first catalog descriptor;
- a live view without a proven future locator uses a separate discriminator
  and reports `IdentityQuality::Approximate`.

The catalog ordinal is segment-global. Counter intervals and state changes are
derived facts, not retained event observations. Event payloads preserve the
fields already retained by typed PostgreSQL log rows; machine kind codes do not
imply diagnosis. In particular, a raw signal does not prove OOM.

## Reductions and health

Counter pairs require one series, forward time, one reset epoch, and no known
gap. A pair is owned by its current sample; reductions record when its evidence
crosses the bucket boundary. Ratios require identical numerator and denominator
pair boundaries.

Gauge inputs reject non-finite values. A bounded gauge reduction retains its
canonical sample set, so merging partitions and reducing the same samples
produce the same mean. Zero-order hold is explicit and stops at known gaps.

Health scoring requires every applicable required factor to have an explicit
penalty and strict full-interval coverage. Partial coverage, loss, an assumed
historical period, a cadence boundary, or unknown exactness keeps the numeric
score absent. Policy-validated floor evidence can make the state `Critical`
without inventing a numeric zero. Downsampling selects a floor cell first;
otherwise it selects the minimum numeric cell with deterministic tie-breaks.

## Bounds and failures

Sparse count keys, counter pairs, gauge samples, returned observations, and
oracle-returned coverage spans have caller-supplied limits. Checked overflow
and limit failures are typed. Oracle queries return observations, counts, and
coverage from one pinned adapter call. `MemoryOracle` is a fixture adapter over
decoded records; production raw and index adapters are not implemented here.

See [`src/lib.rs`](src/lib.rs) for the public surface.
