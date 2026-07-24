//! Web orchestration for the timeline overview index.
//!
//! This module owns the atomic [`IndexView`] the overview endpoints read and
//! assembles it from a store snapshot. It orchestrates publication, querying,
//! and serialization but never computes health or notable semantics itself:
//! those live in `kronika-analytics` and are called here.

pub(crate) mod cache;
pub(crate) mod cursor;
pub(crate) mod handlers;
pub(crate) mod health;
pub(crate) mod live;
pub(crate) mod view;

pub(crate) use live::OverviewIndex;
