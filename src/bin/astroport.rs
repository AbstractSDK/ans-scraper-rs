use std::collections::{HashMap, HashSet};
use std::fmt::format;
use std::path::Path;
use std::sync::Arc;

use abstract_boot::Abstract;
use abstract_core::{
    objects::{
        PoolMetadata,
        PoolType,
        gov_type::GovernanceDetails,
        pool_id::UncheckedPoolAddress
    },
    abstract_token::QueryMsgFns as Cw20QueryMsgFns,
};
use abstract_core::objects::AssetEntry;
use abstract_core::objects::oracle::Oracle;
use astroport::{
    asset::AssetInfo,
    factory::{PairsResponse, PairType, QueryMsgFns}
};
use clap::Parser;
use cosmwasm_std::{Addr, StdResult};
use cw20::TokenInfoResponse;
use cw_asset::AssetInfoUnchecked;
use cw20_base_orch::msg::{InstantiateMsg as Cw20InstantiateMsg, ExecuteMsg as Cw20ExecuteMsg, QueryMsg as Cw20QueryMsg};
use cw20_base_orch::state::TokenInfo;

use cw_orch::{ContractInstance, DaemonBuilder, DaemonChannel, networks::{ChainInfo, NetworkInfo}, Daemon, networks::parse_network, queriers::DaemonQuerier, queriers::Ibc, Contract};
use cw_orch::queriers::CosmWasm;
use reqwest::Error;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use ibc_chain_registry::asset_list::{Asset, AssetList, DenomUnit};
use ibc_chain_registry::chain::{ChainData, Grpc};
use ibc_chain_registry::constants::ALL_CHAINS;
use ibc_chain_registry::error::RegistryError;
use ibc_chain_registry::fetchable::Fetchable;

pub const ABSTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cw_orch::contract(In)]

#[derive(Serialize, Deserialize, Debug)]
struct JSONResponse(HashMap<String, String>);

pub async fn fetch_astroport_address(url: &str, key: &str) -> Result<String, Error> {
    let response_text = reqwest::get(url).await?.text().await?;

    let lines = response_text.lines().collect::<Vec<_>>();
    let mut json_map = HashMap::new();

    // We parse the json manually because the astroport team does not ensure that their json is incorrect 🙃
    for line in lines {
        if line.trim().is_empty() || line.trim().starts_with('{') || line.trim().starts_with('}') {
            continue;
        }

        let parts = line.split(':').collect::<Vec<_>>();
        if parts.len() == 2 {
            let key = parts[0].trim().trim_matches('"').to_string();
            let value = parts[1]
                .trim()
                .trim_matches(',')
                .trim_matches('"')
                .to_string();
            json_map.insert(key, value);
        }
    }

    let key_address = json_map
        .get(key)
        .unwrap_or_else(|| panic!("{} not found in JSON", key));

    Ok(key_address.to_string())
}

const ASTROPORT_PHOENIX_ADDRS: &'static str = "https://raw.githubusercontent.com/astroport-fi/astroport-changelog/main/terra-2/phoenix-1/core_phoenix.json";
const ASTROPORT_PISCO_ADDRS: &'static str = "https://raw.githubusercontent.com/astroport-fi/astroport-changelog/main/terra-2/pisco-1/core_pisco.json";

const DEX_NAME: &str = "astroport";

fn fetch_asset_lists(rt: Runtime) -> anyhow::Result<Vec<AssetList>> {
    // check for cache dir
    if !Path::new("cache/asset_lists").exists() {
        std::fs::create_dir_all("cache/asset_lists")?;
    }


    let mut lists = Vec::with_capacity(ALL_CHAINS.len());
    for chain in ALL_CHAINS {
        // check cache
        let file_name = format!("cache/asset_lists/{}.json", chain);
        if Path::new(&file_name).exists() {
            let json = std::fs::read_to_string(file_name)?;
            let list: AssetList = serde_json::from_str(&json)?;
            lists.push(list);
            continue;
        }

        let list = rt.block_on(AssetList::fetch(chain.to_string(), None))?;
        let json = serde_json::to_string(&list)?;
        std::fs::write(file_name, json)?;
        lists.push(list);
    }

    Ok(lists)
}

/// Script that registers the first Account in abstract (our Account)
pub fn astroport_ans(network: ChainInfo) -> anyhow::Result<()> {
    // let network = LOCAL_JUNO;
    let rt = Runtime::new()?;

    let chain = Daemon::builder()
        .chain(network)
        .handle(rt.handle())
        .build()
        .unwrap();

    let chain_ans_prefix = match network.chain_id {
        "phoenix-1" => "terra2",
        "pisco-1" => "terra2",
        _ => panic!("Network not supported"),
    };

    let asset_lists = fetch_asset_lists(rt)?;

    let ibc = Ibc::new(chain.state.grpc_channel.clone());

    let url = match (network.chain_id).to_string().as_str() {
        "phoenix-1" => ASTROPORT_PHOENIX_ADDRS,
        "pisco-1" => ASTROPORT_PISCO_ADDRS,
        _ => panic!("Network not supported"),
    };

    let factory_address = rt.block_on(fetch_astroport_address(url, "factory_address"))?;

    let mut astro_factory =
        astroport::factory::AstroportFactory::new("astroport:factory", chain.clone());
    astro_factory
        .as_instance_mut()
        .set_address(&Addr::unchecked(factory_address));

    let mut all_pairs = vec![];
    let mut start_after_pair = None;
    loop {
        let PairsResponse { mut pairs } = astro_factory.pairs(None, start_after_pair)?;
        if pairs.is_empty() {
            break;
        }
        all_pairs.append(&mut pairs);
        start_after_pair = all_pairs.last().map(|p| p.asset_infos);
    }

    let all_asset_infos: HashSet<AssetInfo> = all_pairs
        .into_iter()
        .flat_map(|p| p.asset_infos.into_iter())
        .collect();

    let mut asset_info_to_name: HashMap<AssetInfo, String> = HashMap::new();

    // threshhold for
    let ans_assets_to_add: Vec<(String, AssetInfoUnchecked)> = all_asset_infos
        .into_iter()
        .map(|asset_info| {
            let (name, unchecked_info) = match asset_info.clone() {

                AssetInfo::Token { contract_addr } => {
                    let entry = rt.block_on(cw20_asset_entry(chain, chain_ans_prefix, &contract_addr))?;

                    (entry, AssetInfoUnchecked::cw20(contract_addr))
                }
                AssetInfo::NativeToken { denom } => {
                    let entry = rt.block_on(native_asset_entry(chain, denom.clone(), &asset_lists));
                    let entry = entry.unwrap_or_else(|| {
                        panic!("Native asset not found: {}", denom);
                    });
                    (entry, AssetInfoUnchecked::native(denom))
                },
            };

            asset_info_to_name.insert(asset_info, name.clone());

            Ok((name, unchecked_info))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let ans_pools_to_add: Vec<(UncheckedPoolAddress, PoolMetadata)> = all_pairs
        .into_iter()
        .map(|pair| {
            let pool_id = UncheckedPoolAddress::contract(pair.contract_addr);

            let pool_type = match pair.pair_type {
                PairType::Stable {} => PoolType::Stable,
                PairType::Xyk {} => PoolType::ConstantProduct,
                PairType::Concentrated {} => PoolType::Weighted,
                PairType::Custom(_) => panic!("Custom pair type not supported"),
            };
            let pool_metadata = PoolMetadata {
                dex: DEX_NAME.to_string(),
                pool_type,
                assets: pair
                    .asset_infos
                    .into_iter()
                    .map(|asset_info| {
                        asset_info_to_name
                            .get(&asset_info)
                            .unwrap_or_else(|| panic!("Asset info not found"))
                            .clone()
                    })
                    .map(AssetEntry::from)
                    .collect::<Vec<_>>(),
            };
            (pool_id, pool_metadata)
        })
        .collect::<Vec<_>>();

    Ok(())
}

async fn cw20_asset_entry(chain: Daemon, chain_ans_prefix: &str, contract_addr: &Addr) -> anyhow::Result<String> {
    let mut cw20 = Contract::new(contract_addr.clone().as_str(), chain.clone()).with_address(Some(contract_addr));

    // get the name
    let info: TokenInfoResponse = cw20.query(&Cw20QueryMsg::TokenInfo {})?;

    let name = info.name;
    Ok(format!("{}>{}", chain_ans_prefix, name))
}

/*
              const denomTrace = await ibcQueryClient.ibc.transfer
                .denomTrace(denom)
                .then(({ denomTrace }) => {
                  if (!denomTrace) {
                    throw new Error(`No denom trace for ${denom}`)
                  }
                  return denomTrace
                })

              // { path: 'transfer/channel-4', baseDenom: 'uxprt' }
              const { baseDenom, path } = denomTrace

              // [ 'transfer', 'channel-4' ]
              const splitPath = path.split('/')

              if (splitPath.length !== 2) {
                console.log(`Skipping ${denom} because path is not 2 in length: ${path}`)
                return
              }

              // ['transfer', 'channel-4']
              const [portId, channelId] = splitPath

              if (portId !== 'transfer') {
                console.warn(`Denom trace path for ${denom} is not transfer, but ${portId}`)
                return
              }

              // persistence>xprt
              const ansName = ChainRegistry.externalChainDenomToAnsName(baseDenom)
 */

/*
  static externalChainDenomToAnsName(searchDenom: string): string {
    let found: { chain: string; symbol: string } | undefined
    for (const list of assets) {
      const { chain_name, assets } = list
      const foundAsset = assets.find((unit) =>
        unit.denom_units.some((unit) => unit.denom === searchDenom)
      )
      if (foundAsset) {
        found = { chain: chain_name, symbol: foundAsset.symbol.toLowerCase() }
        break
      }
    }
    if (!found) {
      throw new NotFoundError(`asset not found for address ${searchDenom}`)
    }
    return AnsName.chainNameIbcAsset(found.chain, found.symbol)
  }
 */
async fn native_asset_entry(chain: Daemon, denom: String, x: &Vec<AssetList>) -> Option<String> {
    let ibc = Ibc::new(chain.state.grpc_channel.clone());

    // { path: 'transfer/channel-4', base_denom: 'uxprt' }
    let denom_trace = ibc.denom_trace(denom.clone()).await.ok()?;

    // transfer/channel-4'
    let path = denom_trace.path;

    // [ 'transfer', 'channel-4' ]
    let mut split_path = path.split('/');

    // if split_path.len() != 2 {
    //     log::warn!("Skipping {} because path is not 2 in length: {}", denom, path);
    //     return None
    // }

    // [ 'transfer', 'channel-4' ]
    let port_id = split_path.next().unwrap();

    if port_id != "transfer" {
        log::warn!("Denom trace path for {} is not transfer, but {}", denom, port_id);
        return None
    }
    // 'xprt'
    let base_denom = denom_trace.base_denom;

    let mut ans_asset = None;

    for asset_list in x {
        let AssetList { chain_name, assets, .. } = asset_list;

        let matching_base_denom = assets.iter().find(|asset| {
            let Asset { symbol, denom_units, .. } = asset;

            denom_units.iter().any(|denom_unit| {
                let DenomUnit { denom: d, .. } = denom_unit;
                d == &base_denom
            })

        });

        if let Some(Asset { symbol, .. }) = matching_base_denom {
            ans_asset = Some(format!("{}>{}", chain_name, symbol));
            break;
        }
    }
    ans_asset
}

#[derive(Parser, Default, Debug)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Network Id to deploy on
    #[arg(short, long)]
    network_id: String,
}

fn main() {
    dotenv().ok();
    env_logger::init();

    use dotenv::dotenv;

    let args = Arguments::parse();

    let network = parse_network(&args.network_id);

    if let Err(ref err) = astroport_ans(network) {
        log::error!("{}", err);
        err.chain()
            .skip(1)
            .for_each(|cause| log::error!("because: {}", cause));

        // The backtrace is not always generated. Try to run this example
        // with `$env:RUST_BACKTRACE=1`.
        //    if let Some(backtrace) = e.backtrace() {
        //        log::debug!("backtrace: {:?}", backtrace);
        //    }

        ::std::process::exit(1);
    }
}
