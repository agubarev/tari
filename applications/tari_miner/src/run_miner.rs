// Copyright 2021. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::{convert::TryFrom, str::FromStr, thread, time::Instant};

use futures::stream::StreamExt;
use log::*;
use tari_app_grpc::{
    authentication::ClientAuthenticationInterceptor,
    tari_rpc::{base_node_client::BaseNodeClient, wallet_client::WalletClient},
};
use tari_common::{
    configuration::bootstrap::{grpc_default_port, ApplicationType},
    exit_codes::{ExitCode, ExitError},
    load_configuration,
    DefaultConfigLoader,
};
use tari_comms::utils::multiaddr::multiaddr_to_socketaddr;
use tari_core::blocks::BlockHeader;
use tari_crypto::ristretto::RistrettoPublicKey;
use tari_utilities::hex::Hex;
use tokio::time::sleep;
use tonic::{
    codegen::InterceptedService,
    transport::{Channel, Endpoint},
};

use crate::{
    cli::Cli,
    config::MinerConfig,
    errors::{err_empty, MinerError},
    miner::{Miner, MiningReport},
    stratum::stratum_controller::controller::Controller,
    utils::{coinbase_request, extract_outputs_and_kernels},
};

pub const LOG_TARGET: &str = "tari::miner::main";
pub const LOG_TARGET_FILE: &str = "tari::logging::miner::main";

type WalletGrpcClient = WalletClient<InterceptedService<Channel, ClientAuthenticationInterceptor>>;

#[allow(clippy::too_many_lines)]
pub async fn start_miner(cli: Cli) -> Result<(), ExitError> {
    let config_path = cli.common.config_path();
    let cfg = load_configuration(config_path.as_path(), true, &cli)?;
    let mut config = MinerConfig::load_from(&cfg).expect("Failed to load config");
    debug!(target: LOG_TARGET_FILE, "{:?}", config);
    setup_grpc_config(&mut config);

    if !config.mining_wallet_address.is_empty() && !config.mining_pool_address.is_empty() {
        let url = config.mining_pool_address.clone();
        let mut miner_address = config.mining_wallet_address.clone();
        let _ = RistrettoPublicKey::from_hex(&miner_address).map_err(|_| {
            ExitError::new(
                ExitCode::ConfigError,
                "Miner is not configured with a valid wallet address.",
            )
        })?;
        if !config.mining_worker_name.is_empty() {
            miner_address += &format!("{}{}", ".", config.mining_worker_name);
        }
        let mut mc = Controller::new(config.num_mining_threads).unwrap_or_else(|e| {
            debug!(target: LOG_TARGET_FILE, "Error loading mining controller: {}", e);
            panic!("Error loading mining controller: {}", e);
        });
        let cc = crate::stratum::controller::Controller::new(&url, Some(miner_address), None, None, mc.tx.clone())
            .unwrap_or_else(|e| {
                debug!(
                    target: LOG_TARGET_FILE,
                    "Error loading stratum client controller: {:?}", e
                );
                panic!("Error loading stratum client controller: {:?}", e);
            });
        mc.set_client_tx(cc.tx.clone());

        let _join_handle = thread::Builder::new()
            .name("client_controller".to_string())
            .spawn(move || {
                cc.run();
            });

        mc.run()
            .await
            .map_err(|err| ExitError::new(ExitCode::UnknownError, format!("Stratum error: {:?}", err)))?;

        Ok(())
    } else {
        let (mut node_conn, mut wallet_conn) = connect(&config).await.map_err(|e| {
            ExitError::new(
                ExitCode::GrpcError,
                format!("Could not connect to wallet or base node: {}", e),
            )
        })?;

        let mut blocks_found: u64 = 0;
        loop {
            debug!(target: LOG_TARGET, "Starting new mining cycle");
            match mining_cycle(&mut node_conn, &mut wallet_conn, &config, &cli).await {
                err @ Err(MinerError::GrpcConnection(_)) | err @ Err(MinerError::GrpcStatus(_)) => {
                    // Any GRPC error we will try to reconnect with a standard delay
                    error!(target: LOG_TARGET, "Connection error: {:?}", err);
                    loop {
                        info!(target: LOG_TARGET, "Holding for {:?}", config.wait_timeout());
                        sleep(config.wait_timeout()).await;
                        match connect(&config).await {
                            Ok((nc, wc)) => {
                                node_conn = nc;
                                wallet_conn = wc;
                                break;
                            },
                            Err(err) => {
                                error!(target: LOG_TARGET, "Connection error: {:?}", err);
                                continue;
                            },
                        }
                    }
                },
                Err(MinerError::MineUntilHeightReached(h)) => {
                    warn!(
                        target: LOG_TARGET,
                        "Prescribed blockchain height {} reached. Aborting ...", h
                    );
                    return Ok(());
                },
                Err(MinerError::MinerLostBlock(h)) => {
                    warn!(
                        target: LOG_TARGET,
                        "Height {} already mined by other node. Restarting ...", h
                    );
                },
                Err(err) => {
                    error!(target: LOG_TARGET, "Error: {:?}", err);
                    sleep(config.wait_timeout()).await;
                },
                Ok(submitted) => {
                    info!(target: LOG_TARGET, "💰 Found block");
                    if submitted {
                        blocks_found += 1;
                    }
                    if let Some(max_blocks) = cli.miner_max_blocks {
                        if blocks_found >= max_blocks {
                            return Ok(());
                        }
                    }
                },
            }
        }
    }
}

async fn connect(config: &MinerConfig) -> Result<(BaseNodeClient<Channel>, WalletGrpcClient), MinerError> {
    let base_node_addr = format!(
        "http://{}",
        multiaddr_to_socketaddr(
            &config
                .base_node_grpc_address
                .clone()
                .expect("no base node grpc address found"),
        )?
    );
    info!(target: LOG_TARGET, "🔗 Connecting to base node at {}", base_node_addr);
    let node_conn = BaseNodeClient::connect(base_node_addr).await?;

    let wallet_conn = match connect_wallet(config).await {
        Ok(client) => client,
        Err(e) => {
            error!(target: LOG_TARGET, "Could not connect to wallet");
            error!(
                target: LOG_TARGET,
                "Is its grpc running? try running it with `--enable-grpc` or enable it in config"
            );
            return Err(e);
        },
    };

    Ok((node_conn, wallet_conn))
}

async fn connect_wallet(config: &MinerConfig) -> Result<WalletGrpcClient, MinerError> {
    let wallet_addr = format!(
        "http://{}",
        multiaddr_to_socketaddr(
            &config
                .wallet_grpc_address
                .clone()
                .expect("Wallet grpc address not found")
        )?
    );
    info!(target: LOG_TARGET, "👛 Connecting to wallet at {}", wallet_addr);
    let channel = Endpoint::from_str(&wallet_addr)?.connect().await?;
    let wallet_conn = WalletClient::with_interceptor(
        channel,
        ClientAuthenticationInterceptor::create(&config.wallet_grpc_authentication)?,
    );

    Ok(wallet_conn)
}

async fn mining_cycle(
    node_conn: &mut BaseNodeClient<Channel>,
    wallet_conn: &mut WalletGrpcClient,
    config: &MinerConfig,
    cli: &Cli,
) -> Result<bool, MinerError> {
    debug!(target: LOG_TARGET, "Getting new block template");
    let template = node_conn
        .get_new_block_template(config.pow_algo_request())
        .await?
        .into_inner();
    let mut block_template = template
        .new_block_template
        .clone()
        .ok_or_else(|| err_empty("new_block_template"))?;

    if config.mine_on_tip_only {
        debug!(
            target: LOG_TARGET,
            "Checking if base node is synced, because mine_on_tip_only is true"
        );
        let height = block_template
            .header
            .as_ref()
            .ok_or_else(|| err_empty("header"))?
            .height;
        validate_tip(node_conn, height, cli.mine_until_height).await?;
    }

    debug!(target: LOG_TARGET, "Getting coinbase");
    let request = coinbase_request(&template, config.coinbase_extra.as_bytes().to_vec())?;
    let coinbase = wallet_conn.get_coinbase(request).await?.into_inner();
    let (output, kernel) = extract_outputs_and_kernels(coinbase)?;
    let body = block_template
        .body
        .as_mut()
        .ok_or_else(|| err_empty("new_block_template.body"))?;
    body.outputs.push(output);
    body.kernels.push(kernel);
    let target_difficulty = template
        .miner_data
        .ok_or_else(|| err_empty("miner_data"))?
        .target_difficulty;

    debug!(target: LOG_TARGET, "Asking base node to assemble the MMR roots");
    let block_result = node_conn.get_new_block(block_template).await?.into_inner();
    let block = block_result.block.ok_or_else(|| err_empty("block"))?;
    let header = block.clone().header.ok_or_else(|| err_empty("block.header"))?;

    debug!(target: LOG_TARGET, "Initializing miner");
    let mut reports = Miner::init_mining(header.clone(), target_difficulty, config.num_mining_threads, false);
    let mut reporting_timeout = Instant::now();
    let mut block_submitted = false;
    while let Some(report) = reports.next().await {
        if let Some(header) = report.header.clone() {
            let mut submit = true;
            if let Some(min_diff) = cli.miner_min_diff {
                if report.difficulty < min_diff {
                    submit = false;
                    debug!(
                        target: LOG_TARGET_FILE,
                        "Mined difficulty {} below minimum difficulty {}. Not submitting.", report.difficulty, min_diff
                    );
                }
            }
            if let Some(max_diff) = cli.miner_max_diff {
                if report.difficulty > max_diff {
                    submit = false;
                    debug!(
                        target: LOG_TARGET_FILE,
                        "Mined difficulty {} greater than maximum difficulty {}. Not submitting.",
                        report.difficulty,
                        max_diff
                    );
                }
            }
            if submit {
                // Mined a block fitting the difficulty
                let block_header = BlockHeader::try_from(header.clone()).map_err(MinerError::Conversion)?;
                debug!(
                    target: LOG_TARGET,
                    "Miner found block header {} with difficulty {:?}", block_header, report.difficulty,
                );
                let mut mined_block = block.clone();
                mined_block.header = Some(header);
                // 5. Sending block to the node
                node_conn.submit_block(mined_block).await?;
                block_submitted = true;
                break;
            } else {
                display_report(&report, config.num_mining_threads).await;
            }
        } else {
            display_report(&report, config.num_mining_threads).await;
        }
        if config.mine_on_tip_only && reporting_timeout.elapsed() > config.validate_tip_interval() {
            validate_tip(node_conn, report.height, cli.mine_until_height).await?;
            reporting_timeout = Instant::now();
        }
    }

    // Not waiting for threads to stop, they should stop in a short while after `reports` dropped
    Ok(block_submitted)
}

pub async fn display_report(report: &MiningReport, num_mining_threads: usize) {
    let hashrate = report.hashes as f64 / report.elapsed.as_micros() as f64;
    info!(
        target: LOG_TARGET,
        "⛏ Miner {:0>2} reported {:.2}MH/s with total {:.2}MH/s over {} threads. Height: {}. Target: {})",
        report.miner,
        hashrate,
        hashrate * num_mining_threads as f64,
        num_mining_threads,
        report.height,
        report.target_difficulty,
    );
}

/// If config
async fn validate_tip(
    node_conn: &mut BaseNodeClient<Channel>,
    height: u64,
    mine_until_height: Option<u64>,
) -> Result<(), MinerError> {
    let tip = node_conn
        .get_tip_info(tari_app_grpc::tari_rpc::Empty {})
        .await?
        .into_inner();
    let longest_height = tip.clone().metadata.unwrap().height_of_longest_chain;
    if let Some(height) = mine_until_height {
        if longest_height >= height {
            return Err(MinerError::MineUntilHeightReached(height));
        }
    }
    if height <= longest_height {
        return Err(MinerError::MinerLostBlock(height));
    }
    if !tip.initial_sync_achieved || tip.metadata.is_none() {
        return Err(MinerError::NodeNotReady);
    }
    if height <= longest_height {
        return Err(MinerError::MinerLostBlock(height));
    }
    Ok(())
}

fn setup_grpc_config(config: &mut MinerConfig) {
    if config.base_node_grpc_address.is_none() {
        config.base_node_grpc_address = Some(
            format!(
                "/ip4/127.0.0.1/tcp/{}",
                grpc_default_port(ApplicationType::BaseNode, config.network)
            )
            .parse()
            .unwrap(),
        );
    }

    if config.wallet_grpc_address.is_none() {
        config.wallet_grpc_address = Some(
            format!(
                "/ip4/127.0.0.1/tcp/{}",
                grpc_default_port(ApplicationType::ConsoleWallet, config.network)
            )
            .parse()
            .unwrap(),
        );
    }
}
