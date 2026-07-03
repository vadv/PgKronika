Feature: Every segment carries replication details (1_016_001, 1_017_001)
  pg_replication_slots rows exist without any consumer, so slot scenarios run
  against slots created during the scenario: a physical slot without reserved
  WAL records NULL for every LSN-derived column, reserving WAL fills them, and
  a logical slot carries its plugin and confirmed flush position. The
  walsender section is checked against `pg_receivewal` streaming from the
  cluster. LSN columns are byte offsets from `0/0`; `retained_bytes` is what
  the slot holds back.

  @pg15 @serial
  Scenario: three slot kinds record their NULL patterns
    Given a fresh database on PostgreSQL 15
    And a physical replication slot "kronika_slot_bare"
    And a physical replication slot "kronika_slot_reserved" reserving WAL
    And a logical replication slot "kronika_slot_logical"
    When the collector snapshots the segment
    Then section 1_017_001 has a replication slot "kronika_slot_bare" with:
      | slot_type           | physical |
      | plugin              | null     |
      | active              | false    |
      | restart_lsn         | null     |
      | confirmed_flush_lsn | null     |
      | retained_bytes      | null     |
      | wal_status          | null     |
    And section 1_017_001 has a replication slot "kronika_slot_reserved" with:
      | slot_type           | physical |
      | plugin              | null     |
      | active              | false    |
      | restart_lsn         | not null |
      | confirmed_flush_lsn | null     |
      | retained_bytes      | not null |
      | wal_status          | reserved |
    And section 1_017_001 has a replication slot "kronika_slot_logical" with:
      | slot_type           | logical  |
      | plugin              | pgoutput |
      | active              | false    |
      | restart_lsn         | not null |
      | confirmed_flush_lsn | not null |
      | retained_bytes      | not null |
      | wal_status          | reserved |

  @pg17 @serial
  Scenario: a pg_receivewal walsender is recorded with its stream state
    Given a fresh database on PostgreSQL 17
    And a WAL receiver streams as application "kronika_bdd_receiver" using slot "kronika_recv_slot"
    When the collector snapshots the segment
    Then section 1_016_001 has a replica row for application "kronika_bdd_receiver" with:
      | usename    | postgres  |
      | state      | streaming |
      | sync_state | async     |
      | sent_lsn   | not null  |
    And section 1_017_001 has a replication slot "kronika_recv_slot" with:
      | slot_type   | physical |
      | active      | true     |
      | restart_lsn | not null |

  @pg16 @serial
  Scenario: an idle primary without replicas writes no walsender rows
    Given a fresh database on PostgreSQL 16
    When the collector snapshots the segment
    Then section 1_016_001 is absent from the segment
