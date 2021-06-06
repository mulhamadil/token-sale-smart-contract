use crate::msg::{BalanceResponse, ConfigResponse, HandleMsg, InitMsg, QueryMsg};
use crate::state::{config, config_read, State, User, UsersStorage};
use cosmwasm_std::{
    to_binary, Api, Binary, Env, Extern, HandleResponse, HumanAddr, InitResponse, Querier,
    StdError, StdResult, Storage, Uint128,
};
use secret_toolkit::snip20;

pub const RESPONSE_BLOCK_SIZE: usize = 256;

pub fn init<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: InitMsg,
) -> StdResult<InitResponse> {
    let state = State {
        accepted_token: msg.accepted_token.clone(),
        admin: env.message.sender.clone(),
        offered_token: msg.offered_token.clone(),
        percent_of_funding_collected: 0,
        sale_end_time: msg.sale_end_time,
        viewing_key: msg.viewing_key.clone(),
    };

    config(&mut deps.storage).save(&state)?;

    // https://github.com/enigmampc/secret-toolkit/tree/master/packages/snip20
    // Register this contract to be able to receive the incentivized token
    // Enable this contract to see it's incentivized token details via viewing key
    let messages = vec![
        snip20::register_receive_msg(
            env.contract_code_hash.clone(),
            None,
            1,
            msg.accepted_token.contract_hash.clone(),
            msg.accepted_token.address.clone(),
        )?,
        snip20::set_viewing_key_msg(
            msg.viewing_key.clone(),
            None,
            RESPONSE_BLOCK_SIZE,
            msg.accepted_token.contract_hash,
            msg.accepted_token.address,
        )?,
        snip20::register_receive_msg(
            env.contract_code_hash,
            None,
            1,
            msg.offered_token.contract_hash.clone(),
            msg.offered_token.address.clone(),
        )?,
        snip20::set_viewing_key_msg(
            msg.viewing_key,
            None,
            RESPONSE_BLOCK_SIZE,
            msg.offered_token.contract_hash,
            msg.offered_token.address,
        )?,
    ];

    Ok(InitResponse {
        messages,
        log: vec![],
    })
}

pub fn handle<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: HandleMsg,
) -> StdResult<HandleResponse> {
    match msg {
        HandleMsg::ReceiveAcceptedTokenCallback { from, amount, .. } => {
            receive_accepted_token_callback(deps, env, from, amount)
        }
        HandleMsg::WithdrawFunding { amount } => withdraw_funding(deps, env, amount),
    }
}

pub fn query<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    env: Env,
    msg: QueryMsg,
) -> StdResult<Binary> {
    match msg {
        QueryMsg::AcceptedTokenAvailable {} => to_binary(&accepted_token_available(deps, env)?),
        QueryMsg::Config {} => to_binary(&public_config(deps)?),
        QueryMsg::OfferedTokenAvailable {} => to_binary(&offered_token_available(deps, env)?),
    }
}

fn accepted_token_available<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    env: Env,
) -> StdResult<BalanceResponse> {
    let state = config_read(&deps.storage).load()?;
    let balance = snip20::balance_query(
        &deps.querier,
        env.contract.address,
        state.viewing_key,
        RESPONSE_BLOCK_SIZE,
        state.accepted_token.contract_hash,
        state.accepted_token.address,
    )?;
    Ok(BalanceResponse {
        amount: balance.amount,
    })
}

fn public_config<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
) -> StdResult<ConfigResponse> {
    let state = config_read(&deps.storage).load()?;
    Ok(ConfigResponse {
        accepted_token: state.accepted_token,
        admin: state.admin,
        offered_token: state.offered_token,
        percent_of_funding_collected: state.percent_of_funding_collected,
        sale_end_time: state.sale_end_time,
    })
}

fn receive_accepted_token_callback<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    from: HumanAddr,
    amount: Uint128,
) -> StdResult<HandleResponse> {
    // Ensure that the sent tokens are from an expected contract address
    let state = config_read(&deps.storage).load()?;
    if env.message.sender != state.accepted_token.address {
        return Err(StdError::generic_err(format!(
            "This token is not supported. Supported: {}, given: {}",
            state.accepted_token.address, env.message.sender
        )));
    }
    // Update user's investment_amount
    let user_human_address_as_string: String = from.to_string();
    let user_human_address_as_bytes: &[u8] = user_human_address_as_string.as_bytes();

    // Check if User already exists
    let mut users_storage = UsersStorage::from_storage(&mut deps.storage);
    let user: Option<User> = users_storage.get_user(user_human_address_as_bytes);
    if user.is_none() {
        users_storage.set_user(
            user_human_address_as_bytes,
            User {
                total_investment: amount,
            },
        );
    } else {
        users_storage.set_user(
            user_human_address_as_bytes,
            User {
                total_investment: user.unwrap().total_investment + amount,
            },
        );
    }

    // Transfer offered token to user
    let messages = vec![snip20::transfer_msg(
        from,
        amount,
        None,
        RESPONSE_BLOCK_SIZE,
        state.offered_token.contract_hash,
        state.offered_token.address,
    )?];

    Ok(HandleResponse {
        messages,
        log: vec![],
        data: None,
    })
}

fn offered_token_available<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    env: Env,
) -> StdResult<BalanceResponse> {
    let state = config_read(&deps.storage).load()?;
    let balance = snip20::balance_query(
        &deps.querier,
        env.contract.address,
        state.viewing_key,
        RESPONSE_BLOCK_SIZE,
        state.offered_token.contract_hash,
        state.offered_token.address,
    )?;
    Ok(BalanceResponse {
        amount: balance.amount,
    })
}

fn withdraw_funding<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    amount: Uint128,
) -> StdResult<HandleResponse> {
    let mut state = config_read(&deps.storage).load()?;

    // Enforce admin
    if env.message.sender != state.admin {
        return Err(StdError::Unauthorized { backtrace: None });
    }

    // Enforce that sale has finished
    if env.block.time < state.sale_end_time {
        return Err(StdError::generic_err(
            "Funding can't be withdrawn until sale has ended.",
        ));
    }

    // Update percent of funding collected
    state.percent_of_funding_collected = 100;
    config(&mut deps.storage).save(&state)?;

    // Transfer accepted token to admin
    let messages = vec![snip20::transfer_msg(
        state.admin,
        amount,
        None,
        RESPONSE_BLOCK_SIZE,
        state.accepted_token.contract_hash,
        state.accepted_token.address,
    )?];

    Ok(HandleResponse {
        messages,
        log: vec![],
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SecretContract;
    use cosmwasm_std::from_binary;
    use cosmwasm_std::testing::MockApi;
    use cosmwasm_std::testing::MockQuerier;
    use cosmwasm_std::testing::MockStorage;
    use cosmwasm_std::testing::{mock_dependencies, mock_env};
    pub const MOCK_ADMIN: &str = "admin";
    pub const MOCK_ACCEPTED_TOKEN_ADDRESS: &str = "sefismartcontractaddress";
    pub const MOCK_ACCEPTED_TOKEN_CONTRACT_HASH: &str = "sefismartcontracthash";
    pub const MOCK_OFFERED_TOKEN_ADDRESS: &str = "btnsmartcontractaddress";
    pub const MOCK_OFFERED_TOKEN_CONTRACT_HASH: &str = "btnsmartcontracthash";

    // === HELPERS ===
    fn init_helper() -> (
        StdResult<InitResponse>,
        Extern<MockStorage, MockApi, MockQuerier>,
    ) {
        let env = mock_env(MOCK_ADMIN, &[]);
        let accepted_token = SecretContract {
            address: HumanAddr::from(MOCK_ACCEPTED_TOKEN_ADDRESS),
            contract_hash: MOCK_ACCEPTED_TOKEN_CONTRACT_HASH.to_string(),
        };
        let offered_token = SecretContract {
            address: HumanAddr::from(MOCK_OFFERED_TOKEN_ADDRESS),
            contract_hash: MOCK_OFFERED_TOKEN_CONTRACT_HASH.to_string(),
        };
        let mut deps = mock_dependencies(20, &[]);
        let msg = InitMsg {
            accepted_token: accepted_token.clone(),
            offered_token: offered_token.clone(),
            sale_end_time: 1622935506,
            viewing_key: "nannofromthegirlfromnowhereisathaidemon?".to_string(),
        };
        (init(&mut deps, env.clone(), msg), deps)
    }

    #[test]
    fn test_public_config() {
        let (_init_result, deps) = init_helper();

        let res = query(&deps, mock_env(MOCK_ADMIN, &[]), QueryMsg::Config {}).unwrap();
        let value: ConfigResponse = from_binary(&res).unwrap();
        // Test response does not include viewing key.
        // Test that the desired fields are returned.
        let accepted_token = SecretContract {
            address: HumanAddr::from(MOCK_ACCEPTED_TOKEN_ADDRESS),
            contract_hash: MOCK_ACCEPTED_TOKEN_CONTRACT_HASH.to_string(),
        };
        let offered_token = SecretContract {
            address: HumanAddr::from(MOCK_OFFERED_TOKEN_ADDRESS),
            contract_hash: MOCK_OFFERED_TOKEN_CONTRACT_HASH.to_string(),
        };
        assert_eq!(
            ConfigResponse {
                accepted_token: accepted_token,
                admin: HumanAddr::from(MOCK_ADMIN),
                offered_token: offered_token,
                percent_of_funding_collected: 0,
                sale_end_time: 1622935506,
            },
            value
        );
    }

    #[test]
    fn test_receive_accepted_token_callback() {
        let (_init_result, mut deps) = init_helper();
        let amount: Uint128 = Uint128(333);
        let from: HumanAddr = HumanAddr::from("someuser");

        // Test that only accepted token is accepted
        let msg = HandleMsg::ReceiveAcceptedTokenCallback {
            amount: amount,
            from: from.clone(),
        };
        let handle_response = handle(
            &mut deps,
            mock_env(MOCK_OFFERED_TOKEN_ADDRESS, &[]),
            msg.clone(),
        );
        assert_eq!(
            handle_response.unwrap_err(),
            StdError::GenericErr {
                msg: format!(
                    "This token is not supported. Supported: {}, given: {}",
                    MOCK_ACCEPTED_TOKEN_ADDRESS, MOCK_OFFERED_TOKEN_ADDRESS
                ),
                backtrace: None
            }
        );

        // When received token is the accepted token
        let handle_response = handle(
            &mut deps,
            mock_env(MOCK_ACCEPTED_TOKEN_ADDRESS, &[]),
            msg.clone(),
        );

        // It sends offered token to user
        let res = handle_response.unwrap();
        assert_eq!(1, res.messages.len());

        // It sets or updates the user's total investment
        let handle_response = handle(
            &mut deps,
            mock_env(MOCK_ACCEPTED_TOKEN_ADDRESS, &[]),
            msg.clone(),
        );
        handle_response.unwrap();
        let users_storage = UsersStorage::from_storage(&mut deps.storage);
        let user: Option<User> = users_storage.get_user(from.to_string().as_bytes());
        assert_eq!(amount + amount, user.unwrap().total_investment);
    }

    #[test]
    fn test_withdraw_funding() {
        let (_init_result, mut deps) = init_helper();
        let amount: Uint128 = Uint128(123);
        //=== When user is not admin
        let msg = HandleMsg::WithdrawFunding { amount: amount };
        let handle_response = handle(&mut deps, mock_env("notanadmin", &[]), msg.clone());
        assert_eq!(
            handle_response.unwrap_err(),
            StdError::Unauthorized { backtrace: None }
        );

        //=== When user is admin
        let env = mock_env(MOCK_ADMIN, &[]);

        //=== when sale has NOT ended yet
        let handle_response = handle(&mut deps, env.clone(), msg.clone());
        assert_eq!(
            handle_response.unwrap_err(),
            StdError::generic_err("Funding can't be withdrawn until sale has ended.")
        );

        // === when sale has ended
        let mut state = config_read(&deps.storage).load().unwrap();
        state.sale_end_time = env.block.time - 3;
        config(&mut deps.storage).save(&state).unwrap();

        //=== When withdrawal is successful
        let handle_response = handle(&mut deps, env, msg);

        // It udpates the percent of fundraising collected
        let res = query(&deps, mock_env(MOCK_ADMIN, &[]), QueryMsg::Config {}).unwrap();
        let value: ConfigResponse = from_binary(&res).unwrap();
        assert_eq!(100, value.percent_of_funding_collected);

        // It sends a request to the offered token contract address to transfer tokens to the admin
        let res = handle_response.unwrap();
        assert_eq!(1, res.messages.len());
    }
}
