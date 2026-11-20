//! Map logical chunks and Reed–Solomon coefficients to physical Linux MD stripe members.
use super::{MAX_MEMBERS, RaidLayout, RaidLevel, coefficient};

impl RaidLayout {
    /// Name accepted by mdadm's --layout option.
    pub fn md_name(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::PqFirst => "parity-first",
            Self::PqLast => "parity-last",
            Self::LeftAsymmetric => "left-asymmetric",
            Self::RightAsymmetric => "right-asymmetric",
            Self::LeftSymmetric => "left-symmetric",
            Self::RightSymmetric => "right-symmetric",
            Self::LeftSymmetric6 => "left-symmetric-6",
            Self::RightSymmetric6 => "right-symmetric-6",
            Self::LeftAsymmetric6 => "left-asymmetric-6",
            Self::RightAsymmetric6 => "right-asymmetric-6",
            Self::ParityFirst6 => "parity-first-6",
            Self::DdfZeroRestart => "ddf-zero-restart",
            Self::DdfNRestart => "ddf-N-restart",
            Self::DdfNContinue => "ddf-N-continue",
        }
    }
    pub const RAID5_LAYOUTS: [Self; 4] = [
        Self::LeftSymmetric,
        Self::RightSymmetric,
        Self::LeftAsymmetric,
        Self::RightAsymmetric,
    ];
    pub const RAID6_LAYOUTS: [Self; 14] = [
        Self::PqLast,
        Self::PqFirst,
        Self::LeftSymmetric,
        Self::RightSymmetric,
        Self::LeftAsymmetric,
        Self::RightAsymmetric,
        Self::LeftSymmetric6,
        Self::RightSymmetric6,
        Self::LeftAsymmetric6,
        Self::RightAsymmetric6,
        Self::ParityFirst6,
        Self::DdfZeroRestart,
        Self::DdfNRestart,
        Self::DdfNContinue,
    ];
}

pub(super) struct Stripe {
    pub p: usize,
    pub q: Option<usize>,
    /// Slots in logical disk order. The exponent order in Q can differ.
    pub data: Vec<usize>,
    pub weights: [u8; MAX_MEMBERS],
}

impl Stripe {
    pub fn new(level: RaidLevel, layout: RaidLayout, n: usize, stripe: u64) -> Self {
        use RaidLayout::*;
        let rotation = (stripe % n as u64) as usize;
        let left = n - 1 - rotation;
        // The logical scan begins at 'start', skipping both parity slots. On an
        // asymmetric stripe it starts at slot zero, even when Q wraps around.
        let (p, q, start) = if level == RaidLevel::Raid5 {
            let p = match layout {
                LeftSymmetric | LeftAsymmetric => left,
                _ => rotation,
            };
            let start = if matches!(layout, LeftSymmetric | RightSymmetric) {
                p + 1
            } else {
                0
            };
            (p, None, start)
        } else {
            let dedicated_rotation = (stripe % (n - 1) as u64) as usize;
            let fixed_q_left = n - 2 - dedicated_rotation;
            let (p, q, start) = match layout {
                PqLast => (n - 2, n - 1, 0),
                PqFirst => (0, 1, 2),
                LeftAsymmetric => (left, (left + 1) % n, 0),
                RightAsymmetric | DdfZeroRestart => (rotation, (rotation + 1) % n, 0),
                LeftSymmetric => (left, (left + 1) % n, left + 2),
                RightSymmetric => (rotation, (rotation + 1) % n, rotation + 2),
                LeftAsymmetric6 => (fixed_q_left, n - 1, 0),
                RightAsymmetric6 => (dedicated_rotation, n - 1, 0),
                LeftSymmetric6 => (fixed_q_left, n - 1, fixed_q_left + 1),
                RightSymmetric6 => (dedicated_rotation, n - 1, dedicated_rotation + 1),
                ParityFirst6 => (0, n - 1, 1),
                DdfNRestart => {
                    let p = (left + n - 1) % n;
                    (p, (p + 1) % n, 0)
                }
                DdfNContinue => (left, (left + n - 1) % n, left + 1),
                Fixed => unreachable!("RAID6 layout validated at open"),
            };
            (p, Some(q), start)
        };
        let is_data = |slot: &usize| *slot != p && Some(*slot) != q;
        let data: Vec<_> = (0..n).map(|i| (start + i) % n).filter(is_data).collect();
        let mut weights = [0u8; MAX_MEMBERS];
        if let Some(q) = q {
            let ddf = matches!(layout, DdfZeroRestart | DdfNRestart | DdfNContinue);
            // Native MD starts the syndrome after Q and skips parity. DDF
            // counts physical slots starting at zero, including the P/Q slots.
            let mut by_slot = [0u8; MAX_MEMBERS];
            if ddf {
                for &slot in &data {
                    by_slot[slot] = coefficient(slot);
                }
            } else {
                for (index, slot) in (1..=n).map(|i| (q + i) % n).filter(is_data).enumerate() {
                    by_slot[slot] = coefficient(index);
                }
            }
            for (j, &slot) in data.iter().enumerate() {
                weights[j] = by_slot[slot];
            }
        }
        Self {
            p,
            q,
            data,
            weights,
        }
    }
}
