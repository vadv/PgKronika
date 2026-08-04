# PR206: Statements forensic detail implementation

1. Add red component and App routing tests for an inline Statements detail.
2. Implement `StatementDetail` with bounded point/history queries, four shared
   temporal lanes, impact equation, SQL, metric matrix and related cards.
3. Route selected desktop Statements rows to the inline workspace and suppress
   the generic dock and overview while it is open.
4. Add EN/RU operator copy and extend deterministic demo data for a useful
   related plan.
5. Extend the 1920×1080 shell verifier, run frontend/Rust gates, visually compare
   with the approved reference, review the diff, publish PR206 and merge only
   after green CI.
