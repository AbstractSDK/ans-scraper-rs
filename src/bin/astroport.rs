use clap::Parser;

use cw_orch::{networks::parse_network, networks::ChainInfo, queriers::DaemonQuerier, Daemon};

use ans_scraper_rs::ChainRegistry;
use tokio::runtime::Runtime;

use ans_scraper_rs::dexes::astroport::AstroportScraper;
use ans_scraper_rs::traits::dex::AssetSource;

pub const ABSTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Script that registers the first Account in abstract (our Account)
pub fn astroport_ans(network: ChainInfo) -> anyhow::Result<()> {
    // let network = LOCAL_JUNO;
    let rt = Runtime::new()?;

    let chain = Daemon::builder()
        .chain(network.clone())
        .handle(rt.handle())
        .build()
        .unwrap();

    let _chain_registry = rt.block_on(ChainRegistry::new())?;

    let mut astroport = rt.block_on(AstroportScraper::new(chain, "terra2"));

    let test = astroport.fetch_asset_infos()?;

    println!("{:?}", test);

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
