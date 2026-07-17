//! Incident identity and canonical key encoding.

use std::cmp::Ordering;
use std::sync::Arc;

use kronika_analytics::Episode;

/// Bump when the canonical byte layout changes.
const KEY_VERSION: u8 = 1;

/// A scalar accepted in a canonical series identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum IdentityValue {
    I64(i64),
    U64(u64),
    Bool(bool),
    Text(String),
}

impl IdentityValue {
    const fn encoded_len(&self) -> Option<usize> {
        match self {
            Self::I64(_) | Self::U64(_) => Some(9),
            Self::Bool(_) => Some(2),
            Self::Text(text) => 9_usize.checked_add(text.len()),
        }
    }

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

fn encode_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A scored anomaly episode plus the identity of the series it belongs to.
pub(crate) struct EnrichedEpisode {
    pub episode: Episode,
    pub reference: EpisodeRefV1,
}

/// Stable reference to one series' anomaly episode. Union rows do not preserve
/// `type_id`, so the key uses the logical section name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeRefV1 {
    pub logical_section: &'static str,
    pub column: &'static str,
    pub identity: Arc<[IdentityValue]>,
    pub start_us: i64,
    pub end_us: i64,
}

impl EpisodeRefV1 {
    fn encoded_len(&self) -> Option<usize> {
        let mut len = 8_usize
            .checked_add(self.logical_section.len())?
            .checked_add(8)?
            .checked_add(self.column.len())?
            .checked_add(8)?
            .checked_add(16)?;
        for value in self.identity.iter() {
            len = len.checked_add(value.encoded_len()?)?;
        }
        Some(len)
    }

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

pub(crate) struct IncidentKeyV1 {
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyTooLarge {
    pub observed: usize,
    pub limit: usize,
}

impl IncidentKeyV1 {
    pub(crate) fn new(
        node_self_id: &str,
        start_us: i64,
        end_us: i64,
        members: &[EpisodeRefV1],
        max_bytes: usize,
    ) -> Result<Self, KeyTooLarge> {
        let mut ordered: Vec<&EpisodeRefV1> = members.iter().collect();
        ordered.sort_unstable();

        let mut encoded_len = 1_usize
            .checked_add(8)
            .and_then(|len| len.checked_add(node_self_id.len()))
            .and_then(|len| len.checked_add(16))
            .and_then(|len| len.checked_add(8))
            .unwrap_or(usize::MAX);
        for member in &ordered {
            encoded_len = encoded_len.saturating_add(member.encoded_len().unwrap_or(usize::MAX));
        }
        if encoded_len > max_bytes {
            return Err(KeyTooLarge {
                observed: encoded_len,
                limit: max_bytes,
            });
        }

        let mut bytes = Vec::with_capacity(encoded_len);
        bytes.push(KEY_VERSION);
        encode_bytes(&mut bytes, node_self_id.as_bytes());
        bytes.extend_from_slice(&start_us.to_be_bytes());
        bytes.extend_from_slice(&end_us.to_be_bytes());
        bytes.extend_from_slice(&(ordered.len() as u64).to_be_bytes());
        for member in ordered {
            member.encode(&mut bytes);
        }
        Ok(Self { bytes })
    }

    pub(crate) fn canonical_bytes(&self) -> &[u8] {
        &self.bytes
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
        let key_a = IncidentKeyV1::new("n", 0, 100, &[a], 1024).expect("within limit");
        let key_b = IncidentKeyV1::new("n", 0, 100, &[b], 1024).expect("within limit");
        assert_eq!(key_a.canonical_bytes(), key_b.canonical_bytes());
    }

    #[test]
    fn length_prefix_prevents_text_boundary_collision() {
        let ab_c = iref("s", &[text("ab"), text("c")], 0, 1);
        let a_bc = iref("s", &[text("a"), text("bc")], 0, 1);
        let ka = IncidentKeyV1::new("n", 0, 1, &[ab_c], 1024).expect("within limit");
        let kb = IncidentKeyV1::new("n", 0, 1, &[a_bc], 1024).expect("within limit");
        assert_ne!(ka.canonical_bytes(), kb.canonical_bytes());
    }

    #[test]
    fn distinct_fields_produce_distinct_keys() {
        let base = || iref("s", &[IdentityValue::U64(1)], 10, 20);
        let bytes = |r: EpisodeRefV1| {
            IncidentKeyV1::new("n", 0, 99, &[r], 1024)
                .expect("within limit")
                .canonical_bytes()
                .to_vec()
        };
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
        let forward =
            IncidentKeyV1::new("n", 0, 10, &[x.clone(), y.clone()], 1024).expect("within limit");
        let reversed = IncidentKeyV1::new("n", 0, 10, &[y, x], 1024).expect("within limit");
        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
    }

    #[test]
    fn node_id_participates_in_the_key() {
        let r = iref("s", &[IdentityValue::I64(1)], 0, 1);
        let one = IncidentKeyV1::new("node-a", 0, 1, std::slice::from_ref(&r), 1024)
            .expect("within limit")
            .canonical_bytes()
            .to_vec();
        let two = IncidentKeyV1::new("node-b", 0, 1, &[r], 1024)
            .expect("within limit")
            .canonical_bytes()
            .to_vec();
        assert_ne!(one, two);
    }

    #[test]
    fn key_carries_the_version_byte() {
        let r = iref("s", &[IdentityValue::I64(1)], 0, 1);
        let key = IncidentKeyV1::new("n", 0, 1, &[r], 1024).expect("within limit");
        assert_eq!(key.canonical_bytes().first(), Some(&KEY_VERSION));
    }

    #[test]
    fn key_size_is_checked_before_allocation() {
        let reference = iref("section", &[text("identity")], 0, 1);
        assert_eq!(
            IncidentKeyV1::new("node", 0, 1, &[reference], 1).err(),
            Some(KeyTooLarge {
                observed: 102,
                limit: 1,
            })
        );
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
