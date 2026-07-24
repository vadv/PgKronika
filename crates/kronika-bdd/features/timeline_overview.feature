@timeline
Feature: Source-scoped timeline facts stay coherent across supported PostgreSQL versions
  A fixed PostgreSQL log fixture must produce one reconciled publication through
  collection, sealed storage, canonical fact extraction, and the HTTP timeline.

  @pg15 @serial
  Scenario: PostgreSQL 15 publishes one reconciled source-scoped timeline
    Given a fresh database on PostgreSQL 15
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then the fixed log facts reconcile through the source-scoped timeline

  @pg16 @serial
  Scenario: PostgreSQL 16 publishes one reconciled source-scoped timeline
    Given a fresh database on PostgreSQL 16
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then the fixed log facts reconcile through the source-scoped timeline

  @pg17 @serial
  Scenario: PostgreSQL 17 publishes one reconciled source-scoped timeline
    Given a fresh database on PostgreSQL 17
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then the fixed log facts reconcile through the source-scoped timeline

  @pg18 @serial
  Scenario: PostgreSQL 18 publishes one reconciled source-scoped timeline
    Given a fresh database on PostgreSQL 18
    And a fixed timeline PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then the fixed log facts reconcile through the source-scoped timeline
