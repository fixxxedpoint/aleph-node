use clap::Parser;
use std::{fs, path::PathBuf};

#[derive(Debug, Parser)]
#[clap(version = "1.0")]
pub struct Config {
    /// URL address(es) of the nodes to send transactions to
    #[clap(long, default_value = "127.0.0.1:9945")]
    pub nodes: Vec<String>,

    /// how many transactions to send
    #[clap(long, default_value = "10000")]
    pub transactions: u64,

    /// what throughput to use (transactions/s)
    #[clap(long, default_value = "1000")]
    pub throughput: u64,

    /// secret phrase : a path to a file or passed on stdin
    #[clap(long)]
    pub phrase: Option<String>,

    /// secret seed of the account keypair passed on stdin
    #[clap(long, conflicts_with_all = &["phrase"])]
    pub seed: Option<String>,

    /// allows to skip accounts initialization process and just attempt to download their nonces
    #[clap(long)]
    pub skip_initialization: bool,

    /// beginning of the integer range used to derive accounts
    #[clap(long, default_value = "0")]
    pub first_account_in_range: u64,

    /// number of threads spawn during the flooding process
    #[clap(long, default_value = "3")]
    pub threads: u64,
}

pub fn read_phrase(phrase: String) -> String {
    let file = PathBuf::from(&phrase);
    if file.is_file() {
        fs::read_to_string(phrase).unwrap().trim_end().to_owned()
    } else {
        phrase
    }
}
