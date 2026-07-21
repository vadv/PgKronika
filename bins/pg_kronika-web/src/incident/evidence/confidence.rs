#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Confidence(u8);

impl Confidence {
    pub(crate) const LOW: Self = Self(0);
    pub(crate) const MEDIUM: Self = Self(1);
    pub(crate) const HIGH: Self = Self(2);

    pub(crate) const fn label(self) -> &'static str {
        match self.0 {
            0 => "low",
            1 => "medium",
            _ => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfidenceCap {
    Low,
    Medium,
    High,
}

impl ConfidenceCap {
    pub(super) const fn confidence(self) -> Confidence {
        match self {
            Self::Low => Confidence::LOW,
            Self::Medium => Confidence::MEDIUM,
            Self::High => Confidence::HIGH,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Lead,
    Amplifier,
    Downstream,
    Coincident,
}

impl Role {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Amplifier => "amplifier",
            Self::Downstream => "downstream",
            Self::Coincident => "coincident",
        }
    }
}
