use cosmwasm_schema::cw_serde;
use cosmwasm_std::Uint128;

/// Unbonding period in seconds
pub type UnbondingPeriod = u64;

#[cw_serde]
pub struct InstantiateMsg {
    /// address of cw20 contract token
    pub cw20_contract: String,
    pub tokens_per_power: Uint128,
    pub min_bond: Uint128,
    pub unbonding_periods: Vec<UnbondingPeriod>,
    /// the maximum number of distributions that can be created
    pub max_distributions: u32,

    // admin can only add/remove hooks and add distributions, not change other parameters
    pub admin: Option<String>,
    /// Address of the account that can call [`ExecuteMsg::QuickUnbond`]
    pub unbonder: Option<String>,
}
