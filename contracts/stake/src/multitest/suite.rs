use std::collections::HashMap;

use anyhow::Result as AnyResult;

use cosmwasm_std::{to_binary, Addr, Coin, Decimal, Empty, StdResult, Uint128};
use cw20::{BalanceResponse, Cw20Coin, Cw20ExecuteMsg, Cw20QueryMsg, MinterResponse};
use cw20_base::msg::InstantiateMsg as Cw20InstantiateMsg;
use cw_controllers::{Claim, ClaimsResponse};
use cw_multi_test::{App, AppResponse, Contract, ContractWrapper, Executor};
use wynd_curve_utils::Curve;
use wyndex::{
    asset::{AssetInfo, AssetInfoExt, AssetInfoValidated, AssetValidated},
    stake::{InstantiateMsg, UnbondingPeriod},
};

use crate::msg::{
    AllStakedResponse, AnnualizedReward, AnnualizedRewardsResponse, BondingInfoResponse,
    BondingPeriodInfo, DelegatedResponse, DistributedRewardsResponse, ExecuteMsg, QueryMsg,
    ReceiveDelegationMsg, RewardsPowerResponse, StakedResponse, UndistributedRewardsResponse,
    WithdrawableRewardsResponse,
};

pub const SEVEN_DAYS: u64 = 604800;

fn contract_stake() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new_with_empty(
        crate::contract::execute,
        crate::contract::instantiate,
        crate::contract::query,
    );

    Box::new(contract)
}

pub(super) fn contract_token() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new_with_empty(
        cw20_base::contract::execute,
        cw20_base::contract::instantiate,
        cw20_base::contract::query,
    );

    Box::new(contract)
}

pub(super) fn juno_power(amount: u128) -> Vec<(AssetInfoValidated, u128)> {
    vec![(AssetInfoValidated::Native("juno".to_string()), amount)]
}

pub(super) fn juno(amount: u128) -> AssetValidated {
    AssetInfoValidated::Native("juno".to_string()).with_balance(amount)
}

pub(super) fn native_token(denom: String, amount: u128) -> AssetValidated {
    AssetInfoValidated::Native(denom).with_balance(amount)
}

#[derive(Debug)]
pub struct SuiteBuilder {
    pub cw20_contract: String,
    pub tokens_per_power: Uint128,
    pub min_bond: Uint128,
    pub unbonding_periods: Vec<UnbondingPeriod>,
    pub admin: Option<String>,
    pub initial_balances: Vec<Cw20Coin>,
    pub native_balances: Vec<(Addr, Coin)>,
}

impl SuiteBuilder {
    pub fn new() -> Self {
        Self {
            cw20_contract: "".to_owned(),
            tokens_per_power: Uint128::new(1000),
            min_bond: Uint128::new(5000),
            unbonding_periods: vec![SEVEN_DAYS],
            admin: None,
            initial_balances: vec![],
            native_balances: vec![],
        }
    }

    pub fn with_native_balances(mut self, denom: &str, balances: Vec<(&str, u128)>) -> Self {
        self.native_balances
            .extend(balances.into_iter().map(|(addr, amount)| {
                (
                    Addr::unchecked(addr),
                    Coin {
                        denom: denom.to_owned(),
                        amount: amount.into(),
                    },
                )
            }));
        self
    }

    pub fn with_initial_balances(mut self, balances: Vec<(&str, u128)>) -> Self {
        let initial_balances = balances
            .into_iter()
            .map(|(address, amount)| Cw20Coin {
                address: address.to_owned(),
                amount: amount.into(),
            })
            .collect::<Vec<Cw20Coin>>();
        self.initial_balances = initial_balances;
        self
    }

    pub fn with_min_bond(mut self, min_bond: u128) -> Self {
        self.min_bond = min_bond.into();
        self
    }

    pub fn with_admin(mut self, admin: &str) -> Self {
        self.admin = Some(admin.to_owned());
        self
    }

    pub fn with_unbonding_periods(mut self, unbonding_periods: Vec<UnbondingPeriod>) -> Self {
        self.unbonding_periods = unbonding_periods;
        self
    }

    #[track_caller]
    pub fn build(self) -> Suite {
        let mut app: App = App::default();
        // provide initial native balances
        app.init_modules(|router, _, storage| {
            // group by address
            let mut balances = HashMap::<Addr, Vec<Coin>>::new();
            for (addr, coin) in self.native_balances {
                let addr_balance = balances.entry(addr).or_default();
                addr_balance.push(coin);
            }

            for (addr, coins) in balances {
                router
                    .bank
                    .init_balance(storage, &addr, coins)
                    .expect("init balance");
            }
        });

        let admin = Addr::unchecked("admin");

        let token_id = app.store_code(contract_token());
        let token_contract = app
            .instantiate_contract(
                token_id,
                admin.clone(),
                &Cw20InstantiateMsg {
                    name: "vesting".to_owned(),
                    symbol: "VEST".to_owned(),
                    decimals: 9,
                    initial_balances: self.initial_balances,
                    mint: Some(MinterResponse {
                        minter: "minter".to_owned(),
                        cap: None,
                    }),
                    marketing: None,
                },
                &[],
                "vesting",
                None,
            )
            .unwrap();

        let stake_id = app.store_code(contract_stake());
        let stake_contract = app
            .instantiate_contract(
                stake_id,
                admin,
                &InstantiateMsg {
                    cw20_contract: token_contract.to_string(),
                    tokens_per_power: self.tokens_per_power,
                    min_bond: self.min_bond,
                    unbonding_periods: self.unbonding_periods,
                    admin: self.admin,
                    max_distributions: 6,
                },
                &[],
                "stake",
                None,
            )
            .unwrap();

        Suite {
            app,
            stake_contract,
            token_contract,
        }
    }
}

pub struct Suite {
    pub app: App,
    stake_contract: Addr,
    token_contract: Addr,
}

impl Suite {
    pub fn stake_contract(&self) -> String {
        self.stake_contract.to_string()
    }

    pub fn token_contract(&self) -> String {
        self.token_contract.to_string()
    }

    // update block's time to simulate passage of time
    pub fn update_time(&mut self, time_update: u64) {
        let mut block = self.app.block_info();
        block.time = block.time.plus_seconds(time_update);
        self.app.set_block(block);
    }

    fn unbonding_period_or_default(&self, unbonding_period: impl Into<Option<u64>>) -> u64 {
        // Use default SEVEN_DAYS unbonding period if none provided
        if let Some(up) = unbonding_period.into() {
            up
        } else {
            SEVEN_DAYS
        }
    }

    // create a new distribution flow for staking
    pub fn create_distribution_flow(
        &mut self,
        sender: &str,
        manager: &str,
        asset: AssetInfo,
        rewards: Vec<(UnbondingPeriod, Decimal)>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.stake_contract.clone(),
            &ExecuteMsg::CreateDistributionFlow {
                manager: manager.to_string(),
                asset,
                rewards,
            },
            &[],
        )
    }

    // call to staking contract by sender
    pub fn delegate(
        &mut self,
        sender: &str,
        amount: u128,
        unbonding_period: impl Into<Option<u64>>,
    ) -> AnyResult<AppResponse> {
        self.delegate_as(sender, amount, unbonding_period, None)
    }

    // call to staking contract by sender
    pub fn delegate_as(
        &mut self,
        sender: &str,
        amount: u128,
        unbonding_period: impl Into<Option<u64>>,
        delegate_as: Option<&str>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.token_contract.clone(),
            &Cw20ExecuteMsg::Send {
                contract: self.stake_contract.to_string(),
                amount: amount.into(),
                msg: to_binary(&ReceiveDelegationMsg::Delegate {
                    unbonding_period: self.unbonding_period_or_default(unbonding_period),
                    delegate_as: delegate_as.map(|s| s.to_string()),
                })?,
            },
            &[],
        )
    }

    // call to staking contract by sender
    pub fn mass_delegate(
        &mut self,
        sender: &str,
        amount: u128,
        unbonding_period: impl Into<Option<u64>>,
        delegate_to: &[(&str, u128)],
    ) -> AnyResult<AppResponse> {
        let delegate_to = delegate_to
            .iter()
            .map(|(a, b)| (a.to_string(), Uint128::new(*b)))
            .collect();

        self.app.execute_contract(
            Addr::unchecked(sender),
            self.token_contract.clone(),
            &Cw20ExecuteMsg::Send {
                contract: self.stake_contract.to_string(),
                amount: amount.into(),
                msg: to_binary(&ReceiveDelegationMsg::MassDelegate {
                    unbonding_period: self.unbonding_period_or_default(unbonding_period),
                    delegate_to,
                })?,
            },
            &[],
        )
    }

    // call to stake contract by sender
    pub fn rebond(
        &mut self,
        sender: &str,
        amount: u128,
        bond_from: impl Into<Option<u64>>,
        bond_to: impl Into<Option<u64>>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.stake_contract.clone(),
            &ExecuteMsg::Rebond {
                tokens: amount.into(),
                bond_from: self.unbonding_period_or_default(bond_from),
                bond_to: self.unbonding_period_or_default(bond_to),
            },
            &[],
        )
    }

    pub fn unbond(
        &mut self,
        sender: &str,
        amount: u128,
        unbonding_period: impl Into<Option<u64>>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.stake_contract.clone(),
            &ExecuteMsg::Unbond {
                tokens: amount.into(),
                unbonding_period: self.unbonding_period_or_default(unbonding_period),
            },
            &[],
        )
    }

    pub fn claim(&mut self, sender: &str) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.stake_contract.clone(),
            &ExecuteMsg::Claim {},
            &[],
        )
    }

    // call to vesting contract
    pub fn transfer(
        &mut self,
        sender: &str,
        recipient: &str,
        amount: impl Into<Uint128>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            self.token_contract.clone(),
            &Cw20ExecuteMsg::Transfer {
                recipient: recipient.into(),
                amount: amount.into(),
            },
            &[],
        )
    }

    pub fn distribute_funds<'s>(
        &mut self,
        executor: &str,
        sender: impl Into<Option<&'s str>>,
        funds: Option<AssetValidated>,
    ) -> AnyResult<AppResponse> {
        let sender = sender.into();

        if let Some(funds) = funds {
            let transfer_msg = funds.into_msg(self.stake_contract.clone())?;
            self.app
                .execute(Addr::unchecked(sender.unwrap_or(executor)), transfer_msg)?;
        }

        self.app.execute_contract(
            Addr::unchecked(executor),
            self.stake_contract.clone(),
            &ExecuteMsg::DistributeRewards {
                sender: sender.map(str::to_owned),
            },
            &[],
        )
    }

    pub fn execute_fund_distribution<'s>(
        &mut self,
        executor: &str,
        sender: impl Into<Option<&'s str>>,
        funds: AssetValidated,
    ) -> AnyResult<AppResponse> {
        let _sender = sender.into();

        self.app.execute_contract(
            Addr::unchecked(executor),
            self.stake_contract.clone(),
            &ExecuteMsg::FundDistribution {
                curve: Curve::saturating_linear((0, funds.amount.u128()), (100, 0)),
            },
            &[Coin {
                denom: funds.info.to_string(),
                amount: funds.amount,
            }],
        )
    }

    // call to staking contract by sender
    pub fn execute_fund_distribution_with_cw20(
        &mut self,
        executor: &str,
        funds: AssetValidated,
        token: Addr,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(executor),
            token,
            &Cw20ExecuteMsg::Send {
                contract: self.stake_contract.to_string(),
                amount: funds.amount,
                msg: to_binary(&ReceiveDelegationMsg::Fund {
                    curve: Curve::saturating_linear((0, funds.amount.u128()), (100, 0)),
                })?,
            },
            &[],
        )
    }

    pub fn withdraw_funds<'s>(
        &mut self,
        executor: &str,
        owner: impl Into<Option<&'s str>>,
        receiver: impl Into<Option<&'s str>>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(executor),
            self.stake_contract.clone(),
            &ExecuteMsg::WithdrawRewards {
                owner: owner.into().map(str::to_owned),
                receiver: receiver.into().map(str::to_owned),
            },
            &[],
        )
    }

    #[allow(dead_code)]
    pub fn delegate_withdrawal(
        &mut self,
        executor: &str,
        delegated: &str,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(executor),
            self.stake_contract.clone(),
            &ExecuteMsg::DelegateWithdrawal {
                delegated: delegated.to_owned(),
            },
            &[],
        )
    }

    pub fn withdrawable_rewards(&self, owner: &str) -> StdResult<Vec<AssetValidated>> {
        let resp: WithdrawableRewardsResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::WithdrawableRewards {
                owner: owner.to_owned(),
            },
        )?;
        Ok(resp.rewards)
    }

    pub fn distributed_funds(&self) -> StdResult<Vec<AssetValidated>> {
        let resp: DistributedRewardsResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::DistributedRewards {},
        )?;
        Ok(resp.distributed)
    }

    pub fn withdrawable_funds(&self) -> StdResult<Vec<AssetValidated>> {
        let resp: DistributedRewardsResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::DistributedRewards {},
        )?;
        Ok(resp.withdrawable)
    }

    pub fn undistributed_funds(&self) -> StdResult<Vec<AssetValidated>> {
        let resp: UndistributedRewardsResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::UndistributedRewards {},
        )?;
        Ok(resp.rewards)
    }

    #[allow(dead_code)]
    pub fn delegated(&self, owner: &str) -> StdResult<Addr> {
        let resp: DelegatedResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::Delegated {
                owner: owner.to_owned(),
            },
        )?;
        Ok(resp.delegated)
    }

    /// returns address' balance of native token
    pub fn query_balance(&self, address: &str, denom: &str) -> StdResult<u128> {
        let resp = self.app.wrap().query_balance(address, denom)?;
        Ok(resp.amount.u128())
    }

    pub fn query_cw20_balance(&self, address: &str, cw20: impl Into<String>) -> StdResult<u128> {
        let balance: BalanceResponse = self.app.wrap().query_wasm_smart(
            cw20,
            &Cw20QueryMsg::Balance {
                address: address.to_owned(),
            },
        )?;
        Ok(balance.balance.u128())
    }

    // returns address' balance on vesting contract
    pub fn query_balance_vesting_contract(&self, address: &str) -> StdResult<u128> {
        let balance: BalanceResponse = self.app.wrap().query_wasm_smart(
            self.token_contract.clone(),
            &Cw20QueryMsg::Balance {
                address: address.to_owned(),
            },
        )?;
        Ok(balance.balance.u128())
    }

    // returns address' balance on vesting contract
    pub fn query_balance_staking_contract(&self) -> StdResult<u128> {
        let balance: BalanceResponse = self.app.wrap().query_wasm_smart(
            self.token_contract.clone(),
            &Cw20QueryMsg::Balance {
                address: self.stake_contract.to_string(),
            },
        )?;
        Ok(balance.balance.u128())
    }

    pub fn query_staked(
        &self,
        address: &str,
        unbonding_period: impl Into<Option<u64>>,
    ) -> StdResult<u128> {
        let staked: StakedResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::Staked {
                address: address.to_owned(),
                unbonding_period: self.unbonding_period_or_default(unbonding_period),
            },
        )?;
        Ok(staked.stake.u128())
    }

    pub fn query_staked_periods(&self) -> StdResult<Vec<BondingPeriodInfo>> {
        let info: BondingInfoResponse = self
            .app
            .wrap()
            .query_wasm_smart(self.stake_contract.clone(), &QueryMsg::BondingInfo {})?;
        Ok(info.bonding)
    }

    pub fn query_all_staked(&self, address: &str) -> StdResult<AllStakedResponse> {
        let all_staked: AllStakedResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::AllStaked {
                address: address.to_owned(),
            },
        )?;
        Ok(all_staked)
    }

    pub fn query_claims(&self, address: &str) -> StdResult<Vec<Claim>> {
        let claims: ClaimsResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::Claims {
                address: address.to_owned(),
            },
        )?;
        Ok(claims.claims)
    }

    pub fn query_annualized_rewards(
        &self,
    ) -> StdResult<Vec<(UnbondingPeriod, Vec<AnnualizedReward>)>> {
        let apr: AnnualizedRewardsResponse = self
            .app
            .wrap()
            .query_wasm_smart(self.stake_contract.clone(), &QueryMsg::AnnualizedRewards {})?;
        Ok(apr.rewards)
    }

    pub fn query_rewards_power(&self, address: &str) -> StdResult<Vec<(AssetInfoValidated, u128)>> {
        let rewards: RewardsPowerResponse = self.app.wrap().query_wasm_smart(
            self.stake_contract.clone(),
            &QueryMsg::RewardsPower {
                address: address.to_owned(),
            },
        )?;

        Ok(rewards
            .rewards
            .into_iter()
            .map(|(a, p)| (a, p.u128()))
            .filter(|(_, p)| *p > 0)
            .collect())
    }

    pub fn query_total_rewards_power(&self) -> StdResult<Vec<(AssetInfoValidated, u128)>> {
        let rewards: RewardsPowerResponse = self
            .app
            .wrap()
            .query_wasm_smart(self.stake_contract.clone(), &QueryMsg::TotalRewardsPower {})?;

        Ok(rewards
            .rewards
            .into_iter()
            .map(|(a, p)| (a, p.u128()))
            .filter(|(_, p)| *p > 0)
            .collect())
    }
}
