@local_join
Feature: Unix-socket collection emits local PostgreSQL join facts
  A collector connected through the test cluster's Unix socket records the
  PostgreSQL storage-mount and process-cgroup sections in its sealed segment.

  @serial
  Scenario Outline: local join sections are present on PostgreSQL <major>
    Given a fresh database on PostgreSQL <major>
    And the collector connects through the PostgreSQL Unix socket
    When the collector snapshots the segment
    Then section pg_process_cgroup_memory is non-empty
    And section 1_036_002 is non-empty

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
