#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coins, to_binary, Addr, Binary, CosmosMsg, Deps, DepsMut, Env, MessageInfo, Response,
    StdResult, Uint128, WasmMsg,
};
use cw2::set_contract_version;
use cw20::Cw20ExecuteMsg;

use cw_placeholder::contract::CONTRACT_NAME as PLACEHOLDER_CONTRACT_NAME;
use wynd_curve_utils::ScalableCurve;
use wyndex::asset::{AssetInfoValidated, AssetValidated};
use wyndex_stake::msg::{
    ExecuteMsg as StakeExecuteMsg, ReceiveDelegationMsg as StakeReceiveDelegationMsg,
};

use crate::error::ContractError;
use crate::msg::{AdapterQueryMsg, ExecuteMsg, InstantiateMsg, MigrateMsg};
use crate::state::{Config, CONFIG};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:gauge-adapter";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let config = Config {
        factory: deps.api.addr_validate(&msg.factory)?,
        owner: deps.api.addr_validate(&msg.owner)?,
        rewards_asset: msg.rewards_asset.validate(deps.api)?,
        distribution_curve: ScalableCurve::linear((0, 100), (msg.epoch_length, 0)),
    };
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::UpdateRewards { amount } => execute::update_rewards(deps, info.sender, amount),
    }
}

mod execute {
    use super::*;

    pub fn update_rewards(
        deps: DepsMut,
        sender: Addr,
        new_amount: Uint128,
    ) -> Result<Response, ContractError> {
        let mut config = CONFIG.load(deps.storage)?;
        if sender != config.owner {
            return Err(ContractError::Unauthorized {});
        }

        config.rewards_asset.amount = new_amount;
        CONFIG.save(deps.storage, &config)?;

        Ok(Response::new()
            .add_attribute("update", "rewards")
            .add_attribute("asset", config.rewards_asset.info.to_string())
            .add_attribute("amount", new_amount.to_string()))
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: AdapterQueryMsg) -> StdResult<Binary> {
    match msg {
        AdapterQueryMsg::Config {} => to_binary(&CONFIG.load(deps.storage)?),
        AdapterQueryMsg::AllOptions {} => to_binary(&query::all_options(deps)?),
        AdapterQueryMsg::CheckOption { option } => to_binary(&query::check_option(deps, option)?),
        AdapterQueryMsg::SampleGaugeMsgs { selected } => {
            to_binary(&query::sample_gauge_msgs(deps, env, selected)?)
        }
    }
}

mod query {
    use cosmwasm_std::Decimal;

    use crate::{
        msg::{AllOptionsResponse, CheckOptionResponse, SampleGaugeMsgsResponse},
        querier::{query_pairs, query_validate_staking_address},
        state::CONFIG,
    };

    use super::*;

    pub fn all_options(deps: Deps) -> StdResult<AllOptionsResponse> {
        let config = CONFIG.load(deps.storage)?;
        Ok(AllOptionsResponse {
            options: query_pairs(&deps.querier, config.factory)?
                .pairs
                .into_iter()
                .map(|p| p.staking_addr.to_string())
                .collect(),
        })
    }

    pub fn check_option(deps: Deps, option: String) -> StdResult<CheckOptionResponse> {
        let config = CONFIG.load(deps.storage)?;
        Ok(CheckOptionResponse {
            valid: query_validate_staking_address(&deps.querier, config.factory, option)?,
        })
    }

    pub fn sample_gauge_msgs(
        deps: Deps,
        env: Env,
        selected: Vec<(String, Decimal)>,
    ) -> StdResult<SampleGaugeMsgsResponse> {
        let Config {
            factory: _,
            owner: _,
            rewards_asset,
            distribution_curve,
        } = CONFIG.load(deps.storage)?;
        Ok(SampleGaugeMsgsResponse {
            execute: selected
                .into_iter()
                .flat_map(|(option, weight)| {
                    let rewards_asset = AssetValidated {
                        info: rewards_asset.info.clone(),
                        amount: rewards_asset.amount * weight,
                    };
                    create_distribute_msgs(&env, rewards_asset, option, distribution_curve.clone())
                        .unwrap()
                })
                .collect(),
        })
    }
}

/// Creates the necessary messages to distribute the given asset to the given staking contract
fn create_distribute_msgs(
    _env: &Env,
    asset: AssetValidated,
    staking_contract: String,
    curve: ScalableCurve,
) -> Result<Vec<CosmosMsg>, ContractError> {
    if asset.amount.is_zero() {
        return Ok(vec![]);
    }
    match &asset.info {
        AssetInfoValidated::Token(_) => Ok(vec![WasmMsg::Execute {
            contract_addr: asset.info.to_string(),
            msg: to_binary(&Cw20ExecuteMsg::Send {
                contract: staking_contract,
                amount: asset.amount,
                msg: to_binary(&StakeReceiveDelegationMsg::Fund {
                    curve: curve.scale(asset.amount),
                })?,
            })?,
            funds: vec![],
        }
        .into()]),
        AssetInfoValidated::Native(denom) => {
            let funds = coins(asset.amount.u128(), denom);
            Ok(vec![WasmMsg::Execute {
                contract_addr: staking_contract,
                msg: to_binary(&StakeExecuteMsg::FundDistribution {
                    curve: curve.scale(asset.amount),
                })?,
                funds,
            }
            .into()])
        }
    }
}

/// Manages the contract migration.
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, env: Env, msg: MigrateMsg) -> Result<Response, ContractError> {
    match msg {
        MigrateMsg::Init(msg) => {
            // Enforce previous contract name was crates.io:cw-placeholder
            let ver = cw2::get_contract_version(deps.storage)?;
            if ver.contract != PLACEHOLDER_CONTRACT_NAME {
                return Err(ContractError::NotPlaceholder);
            }

            // Gather contract info to pass admin
            let contract_info = deps
                .querier
                .query_wasm_contract_info(env.contract.address.clone())?;
            let sender = deps.api.addr_validate(&contract_info.admin.unwrap())?;

            instantiate(
                deps,
                env,
                MessageInfo {
                    sender,
                    funds: vec![],
                },
                msg,
            )
            .unwrap();
        }
    };

    Ok(Response::new())
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::{
        testing::{mock_dependencies, mock_env, mock_info},
        to_binary, Coin, CosmosMsg, Decimal, WasmMsg,
    };
    use wynd_curve_utils::Curve;

    use super::{execute, instantiate, query};
    use crate::{
        error::ContractError,
        msg::{ExecuteMsg, InstantiateMsg},
        state::CONFIG,
    };
    use wyndex::asset::{Asset, AssetInfo};

    const EPOCH_LENGTH: u64 = 86_400;

    #[test]
    fn proper_initialization() {
        let mut deps = mock_dependencies();
        let amount = 1000u64;
        let msg = InstantiateMsg {
            factory: "factory".to_string(),
            owner: "owner".to_string(),
            rewards_asset: wyndex::asset::Asset {
                info: wyndex::asset::AssetInfo::Native("juno".to_string()),
                amount: amount.into(),
            },
            epoch_length: EPOCH_LENGTH,
        };
        instantiate(deps.as_mut(), mock_env(), mock_info("user", &[]), msg).unwrap();

        // check if the config is stored
        let config = CONFIG.load(deps.as_ref().storage).unwrap();
        assert_eq!(config.factory, "factory");
        assert_eq!(
            config.rewards_asset.info,
            wyndex::asset::AssetInfoValidated::Native("juno".to_string())
        );
        assert_eq!(config.rewards_asset.amount.u128(), 1000);
    }

    #[test]
    fn basic_sample() {
        let mut deps = mock_dependencies();
        let amount = 10_000u64;

        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("user", &[]),
            InstantiateMsg {
                factory: "factory".to_string(),
                owner: "owner".to_string(),
                rewards_asset: wyndex::asset::Asset {
                    info: wyndex::asset::AssetInfo::Native("juno".to_string()),
                    amount: amount.into(),
                },
                epoch_length: EPOCH_LENGTH,
            },
        )
        .unwrap();

        let selected = vec![
            ("juno1555".to_string(), Decimal::permille(416)),
            ("juno1444".to_string(), Decimal::permille(333)),
            ("juno1333".to_string(), Decimal::permille(250)),
        ];
        let res = query::sample_gauge_msgs(deps.as_ref(), mock_env(), selected).unwrap();
        assert_eq!(res.execute.len(), 3);
        assert_eq!(
            res.execute[0],
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: "juno1555".to_string(),
                msg: to_binary(&wyndex_stake::msg::ExecuteMsg::FundDistribution {
                    curve: Curve::saturating_linear((0, 4160u128), (EPOCH_LENGTH, 0)),
                })
                .unwrap(),
                funds: vec![Coin {
                    denom: "juno".to_string(),
                    amount: 4160u128.into()
                }],
            })
        );
        assert_eq!(
            res.execute[1],
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: "juno1444".to_string(),
                msg: to_binary(&wyndex_stake::msg::ExecuteMsg::FundDistribution {
                    curve: Curve::saturating_linear((0, 3330u128), (EPOCH_LENGTH, 0)),
                })
                .unwrap(),
                funds: vec![Coin {
                    denom: "juno".to_string(),
                    amount: 3330u128.into(),
                }],
            })
        );
        assert_eq!(
            res.execute[2],
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: "juno1333".to_string(),
                msg: to_binary(&wyndex_stake::msg::ExecuteMsg::FundDistribution {
                    curve: Curve::saturating_linear((0, 2500u128), (EPOCH_LENGTH, 0)),
                })
                .unwrap(),
                funds: vec![Coin {
                    denom: "juno".to_string(),
                    amount: 2500u128.into(),
                }],
            })
        );
    }

    #[test]
    fn update_rewards() {
        let amount = 2000u128;

        let mut deps = mock_dependencies();
        let msg = InstantiateMsg {
            factory: "factory".to_string(),
            owner: "owner".to_string(),
            rewards_asset: Asset {
                info: AssetInfo::Native("juno".to_string()),
                amount: 1000u128.into(),
            },
            epoch_length: EPOCH_LENGTH,
        };
        instantiate(deps.as_mut(), mock_env(), mock_info("user", &[]), msg).unwrap();

        // If not factory, update fails
        let err = execute(
            deps.as_mut(),
            mock_env(),
            mock_info("user", &[]),
            ExecuteMsg::UpdateRewards {
                amount: amount.into(),
            },
        )
        .unwrap_err();
        assert_eq!(ContractError::Unauthorized {}, err);

        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("factory", &[]),
            ExecuteMsg::UpdateRewards {
                amount: amount.into(),
            },
        )
        .unwrap_err();
        assert_eq!(ContractError::Unauthorized {}, err);

        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("owner", &[]),
            ExecuteMsg::UpdateRewards {
                amount: amount.into(),
            },
        )
        .unwrap();

        // check if the config is stored
        let config = CONFIG.load(deps.as_ref().storage).unwrap();
        assert_eq!(
            config.rewards_asset.info,
            wyndex::asset::AssetInfoValidated::Native("juno".to_string())
        );
        assert_eq!(config.rewards_asset.amount.u128(), 2000);
    }
}
