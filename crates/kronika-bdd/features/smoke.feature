Feature: BDD runner smoke
  Checks that the cucumber suite starts before PostgreSQL scenarios are added.

  Scenario: the runner starts
    Given the harness is running
