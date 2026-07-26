@pgm_compaction @serial
Feature: The current compact PGM survives collection and process boundaries
  The production collector must turn multiple live collection windows into one
  current PGM. The same bytes must survive collector recovery, current-reader
  reopen, sibling OVF construction, and a real web-process restart while every
  supported PostgreSQL major retains its own registered layout.

  Scenario Outline: PostgreSQL <major> publishes and serves one compact two-window PGM
    Given a fresh database on PostgreSQL <major>
    And a time-local fixed-semantics PostgreSQL stderr log fixture
    When the production collector recovers at least two completed windows after an abrupt stop
    Then the sealed file has the one current compact physical PGM contract
    And both stored windows retain the exact PostgreSQL major through the current reader
    And real web processes preserve section diff overview anomaly and incident semantics through OVF restart

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
