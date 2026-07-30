//! `PostgreSQL` statement and plan policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, scalar_entry,
};

pub(super) const PG_STATEMENTS_MILLISECONDS_PER_ROW: CatalogEntry = scalar_entry(
    MetricId::PgStatementsMillisecondsPerRow,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 10.0)),
    Some(boundary(Comparison::AtLeast, 100.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_STATEMENTS_MEAN_TIME_MILLISECONDS: CatalogEntry = scalar_entry(
    MetricId::PgStatementsMeanTimeMilliseconds,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 10.0)),
    Some(boundary(Comparison::AtLeast, 100.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_STATEMENTS_TIME_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgStatementsTimePercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 20.0)),
    Some(boundary(Comparison::AtLeast, 50.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_STATEMENTS_PLAN_TIME_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgStatementsPlanTimePercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 50.0)),
    Some(boundary(Comparison::AtLeast, 80.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_STATEMENTS_PLAN_COUNT: CatalogEntry = scalar_entry(
    MetricId::PgStatementsPlanCount,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 1.0)),
    Some(boundary(Comparison::Above, 3.0)),
    ZeroDisposition::Classify,
);
