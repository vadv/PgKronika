//! Pure domain model for incident analysis: series identity, the canonical
//! episode key, and its deterministic byte encoding. No I/O, no HTTP, no JSON.

use std::cmp::Ordering;
use std::sync::Arc;

use kronika_anomaly::Episode;
use kronika_reader::Value;

/// Encoding version of [`IncidentKeyV1`]. Any change to the byte layout below
/// must bump this so a stored key never silently decodes under new rules.
const KEY_VERSION: u8 = 1;

/// A scalar allowed in a series identity: the order-stable subset of reader
/// [`Value`] that `diff_key` columns actually carry. Floats, timestamps, blobs,
/// lists and `NULL` are not identities and are rejected on conversion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum IdentityValue {
    I64(i64),
    U64(u64),
    Bool(bool),
    Text(String),
}

/// Why a reader [`Value`] cannot stand in an incident identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityReject {
    /// `NULL`, an unresolved id, or the `StrId(0)` sentinel: the entity cannot be
    /// named, so the episode is dropped rather than keyed under a stand-in.
    NullOrUnresolved,
    /// A float, timestamp, blob or list — not an order-stable identity scalar.
    NonScalar,
}

impl IdentityValue {
    /// Convert one reader value, rejecting anything that is not a canonical
    /// identity scalar.
    pub(crate) fn from_value(value: &Value) -> Result<Self, IdentityReject> {
        match value {
            Value::Null => Err(IdentityReject::NullOrUnresolved),
            Value::I64(v) => Ok(Self::I64(*v)),
            Value::U64(v) => Ok(Self::U64(*v)),
            Value::Bool(v) => Ok(Self::Bool(*v)),
            Value::Str(s) => Ok(Self::Text(s.clone())),
            _ => Err(IdentityReject::NonScalar),
        }
    }

    /// Append the tagged, length-delimited encoding of this scalar.
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::I64(v) => {
                out.push(0x01);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Self::U64(v) => {
                out.push(0x02);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Self::Bool(v) => {
                out.push(0x03);
                out.push(u8::from(*v));
            }
            Self::Text(s) => {
                out.push(0x04);
                encode_bytes(out, s.as_bytes());
            }
        }
    }
}

/// Append a `u64` big-endian length followed by the bytes. The length prefix is
/// what keeps `("ab","c")` and `("a","bc")` from encoding to the same key.
fn encode_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A scored anomaly episode plus the identity of the series it belongs to.
pub(crate) struct EnrichedEpisode {
    pub episode: Episode,
    pub reference: EpisodeRefV1,
}

/// Stable, cross-process reference to one series' anomaly episode. `type_id` is
/// deliberately absent: union rows drop layout provenance, so a stable key uses
/// the logical section name instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeRefV1 {
    pub logical_section: &'static str,
    pub column: &'static str,
    pub identity: Arc<[IdentityValue]>,
    pub start_us: i64,
    pub end_us: i64,
}

impl EpisodeRefV1 {
    /// Append the canonical encoding of this reference.
    fn encode(&self, out: &mut Vec<u8>) {
        encode_bytes(out, self.logical_section.as_bytes());
        encode_bytes(out, self.column.as_bytes());
        out.extend_from_slice(&(self.identity.len() as u64).to_be_bytes());
        for value in self.identity.iter() {
            value.encode(out);
        }
        out.extend_from_slice(&self.start_us.to_be_bytes());
        out.extend_from_slice(&self.end_us.to_be_bytes());
    }

    /// Total order: interval first, then section, column, and identity. Used
    /// both to cluster episodes and to canonicalise a key's member list.
    const fn order_key(&self) -> (i64, i64, &'static str, &'static str) {
        (
            self.start_us,
            self.end_us,
            self.logical_section,
            self.column,
        )
    }
}

impl Ord for EpisodeRefV1 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order_key()
            .cmp(&other.order_key())
            .then_with(|| self.identity.iter().cmp(other.identity.iter()))
    }
}

impl PartialOrd for EpisodeRefV1 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Identity of one incident: a resolved node id, the interval, and the sorted
/// set of member episodes. Two incidents with the same members in any input
/// order canonicalise to the same bytes.
pub(crate) struct IncidentKeyV1 {
    node_self_id: String,
    start_us: i64,
    end_us: i64,
    members: Vec<EpisodeRefV1>,
}

impl IncidentKeyV1 {
    /// Build a key, sorting members into canonical order. `node_self_id` must be
    /// the resolved UTF-8 node id; an unresolved id has no place here and the
    /// caller drops the incident instead of substituting one.
    pub(crate) fn new(
        node_self_id: String,
        start_us: i64,
        end_us: i64,
        mut members: Vec<EpisodeRefV1>,
    ) -> Self {
        members.sort();
        Self {
            node_self_id,
            start_us,
            end_us,
            members,
        }
    }

    /// The version-tagged, length-delimited byte encoding of this key.
    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![KEY_VERSION];
        encode_bytes(&mut out, self.node_self_id.as_bytes());
        out.extend_from_slice(&self.start_us.to_be_bytes());
        out.extend_from_slice(&self.end_us.to_be_bytes());
        out.extend_from_slice(&(self.members.len() as u64).to_be_bytes());
        for member in &self.members {
            member.encode(&mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iref(section: &'static str, ids: &[IdentityValue], start: i64, end: i64) -> EpisodeRefV1 {
        EpisodeRefV1 {
            logical_section: section,
            column: "c",
            identity: Arc::from(ids.to_vec()),
            start_us: start,
            end_us: end,
        }
    }

    #[test]
    fn scalar_values_convert_to_identity() {
        assert_eq!(
            IdentityValue::from_value(&Value::I64(-7)),
            Ok(IdentityValue::I64(-7))
        );
        assert_eq!(
            IdentityValue::from_value(&Value::U64(7)),
            Ok(IdentityValue::U64(7))
        );
        assert_eq!(
            IdentityValue::from_value(&Value::Bool(true)),
            Ok(IdentityValue::Bool(true))
        );
        assert_eq!(
            IdentityValue::from_value(&Value::Str("db".to_owned())),
            Ok(IdentityValue::Text("db".to_owned()))
        );
    }

    #[test]
    fn null_is_rejected_as_unresolved() {
        assert_eq!(
            IdentityValue::from_value(&Value::Null),
            Err(IdentityReject::NullOrUnresolved)
        );
    }

    #[test]
    fn float_and_timestamp_are_rejected_as_non_scalar() {
        assert_eq!(
            IdentityValue::from_value(&Value::F64(1.0)),
            Err(IdentityReject::NonScalar)
        );
        assert_eq!(
            IdentityValue::from_value(&Value::Ts(1)),
            Err(IdentityReject::NonScalar)
        );
    }

    #[test]
    fn identity_scalars_order_by_variant_then_value() {
        assert!(IdentityValue::I64(9) < IdentityValue::U64(0));
        assert!(IdentityValue::U64(9) < IdentityValue::Bool(false));
        assert!(IdentityValue::Bool(true) < IdentityValue::Text(String::new()));
        assert!(IdentityValue::I64(-1) < IdentityValue::I64(1));
        assert!(IdentityValue::Text("a".to_owned()) < IdentityValue::Text("b".to_owned()));
    }

    #[test]
    fn encoding_is_deterministic() {
        let a = iref("s", &[IdentityValue::I64(1)], 10, 20);
        let b = iref("s", &[IdentityValue::I64(1)], 10, 20);
        let key_a = IncidentKeyV1::new("n".to_owned(), 0, 100, vec![a]);
        let key_b = IncidentKeyV1::new("n".to_owned(), 0, 100, vec![b]);
        assert_eq!(key_a.canonical_bytes(), key_b.canonical_bytes());
    }

    #[test]
    fn length_prefix_prevents_text_boundary_collision() {
        let ab_c = iref("s", &[text("ab"), text("c")], 0, 1);
        let a_bc = iref("s", &[text("a"), text("bc")], 0, 1);
        let ka = IncidentKeyV1::new("n".to_owned(), 0, 1, vec![ab_c]);
        let kb = IncidentKeyV1::new("n".to_owned(), 0, 1, vec![a_bc]);
        assert_ne!(ka.canonical_bytes(), kb.canonical_bytes());
    }

    #[test]
    fn distinct_fields_produce_distinct_keys() {
        let base = || iref("s", &[IdentityValue::U64(1)], 10, 20);
        let bytes =
            |r: EpisodeRefV1| IncidentKeyV1::new("n".to_owned(), 0, 99, vec![r]).canonical_bytes();
        let baseline = bytes(base());
        let mut other_section = base();
        other_section.logical_section = "t";
        let mut other_start = base();
        other_start.start_us = 11;
        assert_ne!(baseline, bytes(other_section));
        assert_ne!(baseline, bytes(other_start));
    }

    #[test]
    fn member_order_does_not_change_the_key() {
        let x = iref("a", &[IdentityValue::I64(1)], 0, 5);
        let y = iref("b", &[IdentityValue::I64(2)], 3, 8);
        let forward = IncidentKeyV1::new("n".to_owned(), 0, 10, vec![x.clone(), y.clone()]);
        let reversed = IncidentKeyV1::new("n".to_owned(), 0, 10, vec![y, x]);
        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
    }

    #[test]
    fn node_id_participates_in_the_key() {
        let r = iref("s", &[IdentityValue::I64(1)], 0, 1);
        let one = IncidentKeyV1::new("node-a".to_owned(), 0, 1, vec![r.clone()]).canonical_bytes();
        let two = IncidentKeyV1::new("node-b".to_owned(), 0, 1, vec![r]).canonical_bytes();
        assert_ne!(one, two);
    }

    #[test]
    fn key_carries_the_version_byte() {
        let r = iref("s", &[IdentityValue::I64(1)], 0, 1);
        let bytes = IncidentKeyV1::new("n".to_owned(), 0, 1, vec![r]).canonical_bytes();
        assert_eq!(bytes.first(), Some(&KEY_VERSION));
    }

    #[test]
    fn references_order_by_interval_then_identity() {
        let early = iref("s", &[IdentityValue::I64(2)], 0, 10);
        let late = iref("s", &[IdentityValue::I64(1)], 5, 10);
        assert!(
            early < late,
            "earlier start sorts first regardless of identity"
        );

        let id_low = iref("s", &[IdentityValue::I64(1)], 0, 10);
        let id_high = iref("s", &[IdentityValue::I64(2)], 0, 10);
        assert!(id_low < id_high, "equal interval falls back to identity");
    }

    fn text(s: &str) -> IdentityValue {
        IdentityValue::Text(s.to_owned())
    }
}
