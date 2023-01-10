use cosmwasm_std::{Decimal, Uint128};
use wynd_curve_utils::{Curve, PiecewiseLinear, SaturatingLinear};

use crate::state::Config;

pub fn calc_power(cfg: &Config, stake: Uint128, multiplier: Decimal) -> Uint128 {
    if stake < cfg.min_bond {
        Uint128::zero()
    } else {
        stake * multiplier / cfg.tokens_per_power
    }
}

pub trait CurveExt {
    /// Shifts this curve to the right by `x` units.
    fn shift(self, x: u64) -> Curve;
}

impl CurveExt for Curve {
    fn shift(self, x: u64) -> Curve {
        match self {
            c @ Curve::Constant { .. } => c,
            Curve::SaturatingLinear(sl) => sl.shift(x),
            Curve::PiecewiseLinear(pl) => pl.shift(x),
        }
    }
}

impl CurveExt for SaturatingLinear {
    fn shift(mut self, x: u64) -> Curve {
        self.min_x = self.min_x.checked_add(x).unwrap();
        self.max_x = self.max_x.checked_add(x).unwrap();

        Curve::SaturatingLinear(self)
    }
}

impl CurveExt for PiecewiseLinear {
    fn shift(mut self, by: u64) -> Curve {
        for (x, _) in self.steps.iter_mut() {
            *x = x.checked_add(by).unwrap();
        }
        Curve::PiecewiseLinear(self)
    }
}
