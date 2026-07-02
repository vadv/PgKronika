Feature: PostgreSQL matrix smoke
  Every configured PostgreSQL version boots and answers a version query.
  The version number reported by the server must match the major the matrix booted.

  @matrix
  Scenario: every booted major reports a matching server_version_num
    Given the PostgreSQL matrix is booted
    Then each cluster's declared major matches the result of:
      """
      SELECT current_setting('server_version_num')::int / 10000
      """
