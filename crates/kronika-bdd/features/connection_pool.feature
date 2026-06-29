Feature: Collector pools a connection to every database
  The pool opens one connection per non-template database the role may connect
  to, and enumeration excludes templates. Live matrix is PG 15-18.

  Scenario: matrix clusters pool every database
    Given the PostgreSQL matrix is booted
    Then each matrix cluster pools one connection per database
