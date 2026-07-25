@timeline @timeline_web_lifecycle
Feature: Real web processes recover same-stem timeline indexes
  A sealed collector segment is served through actual pg_kronika-web processes.
  Lifecycle cases use isolated owned directories, explicit readiness and
  publication barriers, real HTTP and Prometheus responses, and graceful or
  asserted process exits. No retry sleep decides an outcome.

  @pg15 @serial
  Scenario: PostgreSQL 15 real web process recovers sibling indexes across lifecycle boundaries
    Given a fresh database on PostgreSQL 15
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then a real web process builds the sibling and a new process reuses it without PGM body reads
    And a corrupt sibling is rebuilt atomically and survives another real-process restart
    And every stale descriptor schema extractor registry and lineage sibling is rebuilt
    And a stopped build and temporary sibling residue recover without changing source artifacts
    And a recoverable publication failure uses bounded fallback then becomes a durable restart hit
    And a prior-process cursor expires while ordinary timeline data stays equal
    And a second writer process reports deterministic contention without sidecar corruption

  @pg16 @serial
  Scenario: PostgreSQL 16 real web process recovers sibling indexes across lifecycle boundaries
    Given a fresh database on PostgreSQL 16
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then a real web process builds the sibling and a new process reuses it without PGM body reads
    And a corrupt sibling is rebuilt atomically and survives another real-process restart
    And every stale descriptor schema extractor registry and lineage sibling is rebuilt
    And a stopped build and temporary sibling residue recover without changing source artifacts
    And a recoverable publication failure uses bounded fallback then becomes a durable restart hit
    And a prior-process cursor expires while ordinary timeline data stays equal
    And a second writer process reports deterministic contention without sidecar corruption

  @pg17 @serial
  Scenario: PostgreSQL 17 real web process recovers sibling indexes across lifecycle boundaries
    Given a fresh database on PostgreSQL 17
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then a real web process builds the sibling and a new process reuses it without PGM body reads
    And a corrupt sibling is rebuilt atomically and survives another real-process restart
    And every stale descriptor schema extractor registry and lineage sibling is rebuilt
    And a stopped build and temporary sibling residue recover without changing source artifacts
    And a recoverable publication failure uses bounded fallback then becomes a durable restart hit
    And a prior-process cursor expires while ordinary timeline data stays equal
    And a second writer process reports deterministic contention without sidecar corruption

  @pg18 @serial
  Scenario: PostgreSQL 18 real web process recovers sibling indexes across lifecycle boundaries
    Given a fresh database on PostgreSQL 18
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then a real web process builds the sibling and a new process reuses it without PGM body reads
    And a corrupt sibling is rebuilt atomically and survives another real-process restart
    And every stale descriptor schema extractor registry and lineage sibling is rebuilt
    And a stopped build and temporary sibling residue recover without changing source artifacts
    And a recoverable publication failure uses bounded fallback then becomes a durable restart hit
    And a prior-process cursor expires while ordinary timeline data stays equal
    And a second writer process reports deterministic contention without sidecar corruption
