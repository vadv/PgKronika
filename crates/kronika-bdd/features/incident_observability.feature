@web @incident_observability
Feature: Incident observability distinguishes registration from evaluation
  The collected PostgreSQL store and the incident HTTP route publish exact
  bounded catalog counts, request-specific evaluator admission, and the 24
  unavailable strict entity-join requirements without implying a relation.

  @serial
  Scenario Outline: PostgreSQL <major> publishes active incident observability
    Given a fresh database on PostgreSQL <major>
    And a time-local fixed-semantics PostgreSQL stderr log fixture
    When the collector snapshots the segment
    Then the incident API publishes exact active observability

    @pg15
    Examples: PostgreSQL 15
      | major |
      | 15    |

    @pg16
    Examples: PostgreSQL 16
      | major |
      | 16    |

    @pg17
    Examples: PostgreSQL 17
      | major |
      | 17    |

    @pg18
    Examples: PostgreSQL 18
      | major |
      | 18    |

  @pg17 @serial
  Scenario: no-data incident observability does not claim diagnosis
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then the incident API keeps no-data observability honest
