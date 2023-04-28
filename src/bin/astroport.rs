use std::collections::HashMap;

use clap::Parser;
use cosmwasm_std::Addr;
use cw20::{Cw20QueryMsg, TokenInfoResponse};
use cw_asset::AssetInfo;

use cw_orch::{
    networks::parse_network, networks::ChainInfo, queriers::DaemonQuerier, Contract, CwEnv, Daemon,
};

use ans_scraper_rs::ChainRegistry;
use tokio::runtime::Runtime;

use ans_scraper_rs::dexes::astroport::AstroportScraper;
use ans_scraper_rs::traits::dex::{AssetSource, DexScraper};

pub struct Scraper<Chain: CwEnv> {
    chain: Chain,
    chain_registry: ChainRegistry,
    ans_prefix: String,
    dex_scrapers: Vec<Box<dyn DexScraper>>,
    // TODO: use bimap (not possible because of stupid AssetInfo)
    assets: HashMap<String, AssetInfo>,
}

impl Scraper<Daemon> {
    pub async fn new(chain: Daemon, chain_registry: ChainRegistry) -> Self {
        Self {
            chain: chain.clone(),
            chain_registry,
            // TODO!!!
            ans_prefix: "terra2".to_string(),
            dex_scrapers: vec![Box::new(AstroportScraper::new(chain.clone()).await)],
            assets: Default::default(),
        }
    }

    pub fn scrape(&mut self) -> anyhow::Result<()> {
        self.scrape_assets();

        Ok(())
    }

    fn scrape_assets(&mut self) -> anyhow::Result<()> {
        let mut not_found_assets = vec![];

        // Scrape assets, contracts, and pools from DEX sources
        for dex_scraper in self.dex_scrapers.iter_mut() {
            let _dex_id = dex_scraper.dex_id();
            let asset_infos = dex_scraper.fetch_asset_infos()?;

            for asset_info in asset_infos {
                let (asset_name, _unchecked_info) = match &asset_info {
                    // TODO: check for pre-existence USING THE BIMAP (not possible because of stupid AssetInfo)
                    AssetInfo::Cw20(contract_addr) => {
                        if let Ok(entry) = cw20_asset_entry(
                            self.chain.clone(),
                            self.ans_prefix.as_str(),
                            contract_addr,
                        ) {
                            (entry, asset_info.clone())
                        } else {
                            not_found_assets.push(asset_info.clone());
                            continue;
                        }
                    }
                    AssetInfo::Native(denom) => {
                        if let Some(entry) = self.chain.rt_handle.block_on(
                            self.chain_registry
                                .resolve_native_asset(self.chain.clone(), denom.clone()),
                        ) {
                            (entry, AssetInfo::native(denom.clone()))
                        } else {
                            not_found_assets.push(asset_info.clone());
                            continue;
                        }
                    }
                    _ => {
                        log::warn!("AssetInfo not supported: {:?}", asset_info.clone());
                        not_found_assets.push(asset_info.clone());
                        continue;
                    }
                };
                self.assets.insert(asset_name, asset_info);
            }
        }
        Ok(())
    }
}

/// Fetch a given cw20 asset entry for the chain.
/// TODO: move somewhere
fn cw20_asset_entry(
    chain: Daemon,
    chain_ans_prefix: &str,
    contract_addr: &Addr,
) -> anyhow::Result<String> {
    let cw20 =
        Contract::new(contract_addr.clone().as_str(), chain).with_address(Some(contract_addr));

    // get the name
    let info: TokenInfoResponse = cw20.query(&Cw20QueryMsg::TokenInfo {})?;

    let name = info.symbol.to_ascii_lowercase();
    Ok(format!("{}>{}", chain_ans_prefix, name))
}

/// Script that registers the first Account in abstract (our Account)
pub fn astroport_ans(network: ChainInfo) -> anyhow::Result<()> {
    // let network = LOCAL_JUNO;
    let rt = Runtime::new()?;

    let chain = Daemon::builder()
        .chain(network.clone())
        .handle(rt.handle())
        .build()
        .unwrap();

    // let mut astroport = rt.block_on(AstroportScraper::new(chain, "terra2"));

    let chain_registry = rt.block_on(ChainRegistry::new())?;

    let mut scraper = rt.block_on(Scraper::new(chain, chain_registry));
    scraper.scrape()?;

    // println!("{:?}", test);

    Ok(())
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
