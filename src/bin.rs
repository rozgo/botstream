use async_std::task;
use structopt::StructOpt;

mod webrtc;

use webrtc::{async_main, Args};

fn main() -> Result<(), anyhow::Error> {
    let args = Args::from_args();
    task::block_on(async_main(args))
}
