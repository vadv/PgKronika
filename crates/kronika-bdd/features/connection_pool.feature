Feature: Collector opens per-database pool connections
  The pool keeps one connection for each non-template database that grants
  CONNECT. Template databases are excluded on every configured PostgreSQL major.

  Scenario: matrix clusters expose per-database pool coverage
    Given the PostgreSQL matrix is booted
    Then each matrix cluster opens per-database pool connections
