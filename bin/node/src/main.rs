#[cfg(any(feature = "try-runtime", feature = "runtime-benchmarks"))]
use aleph_node::ExecutorDispatch;
use aleph_node::{new_authority, new_partial, Cli, Subcommand};
#[cfg(any(feature = "try-runtime", feature = "runtime-benchmarks"))]
use aleph_runtime::Block;
use log::{info, warn};
use primitives::HEAP_PAGES;
use sc_cli::{clap::Parser, Database, DatabasePruningMode, PruningParams, SubstrateCli};
use sc_network::config::Role;
use sc_service::{Configuration, PartialComponents};

pub struct PruningConfigValidator {
    pruning_enabled: bool,
    overwritten_pruning: bool,
    invalid_state_pruning_setting: Result<(), u32>,
    invalid_blocks_pruning_setting: Result<(), u32>,
    invalid_database_backend: Result<(), Database>,
}

impl PruningConfigValidator {
    pub fn validate_and_fix_parameters(cli: &mut Cli) -> PruningConfigValidator {
        let overwritten_pruning = Self::pruning_changed(&cli.run.import_params.pruning_params);
        let pruning_enabled = cli.aleph.pruning();

        let mut result = PruningConfigValidator {
            pruning_enabled,
            overwritten_pruning,
            invalid_state_pruning_setting: Ok(()),
            invalid_blocks_pruning_setting: Ok(()),
            invalid_database_backend: Ok(()),
        };

        if !pruning_enabled {
            // We need to override state pruning to our default (archive), as substrate has 256 by default.
            // 256 does not work with our code.
            cli.run.import_params.pruning_params.state_pruning = Some(Self::default_state_pruning());
            cli.run.import_params.pruning_params.blocks_pruning = Self::default_blocks_pruning();
            return result;
        }

        match cli.run.import_params.pruning_params.state_pruning {
            Some(mode) => match mode {
                DatabasePruningMode::Archive | DatabasePruningMode::ArchiveCanonical => {}
                DatabasePruningMode::Custom(max_blocks) => {
                    if max_blocks < Self::minimum_state_pruning_when_pruning_enabled() {
                        result.invalid_state_pruning_setting = Err(max_blocks);
                        cli.run.import_params.pruning_params.state_pruning =
                            Some(DatabasePruningMode::Custom(
                                Self::minimum_state_pruning_when_pruning_enabled(),
                            ));
                    }
                }
            },
            None => {
                cli.run.import_params.pruning_params.state_pruning = Some(
                    DatabasePruningMode::Custom(Self::minimum_state_pruning_when_pruning_enabled()),
                )
            }
        }

        match cli.run.import_params.pruning_params.blocks_pruning {
            DatabasePruningMode::Archive | DatabasePruningMode::ArchiveCanonical => {}
            DatabasePruningMode::Custom(blocks_pruning) => {
                result.invalid_blocks_pruning_setting = Err(blocks_pruning);
                cli.run.import_params.pruning_params.blocks_pruning = Self::default_blocks_pruning();
            }
        }

        match cli.run.import_params.database_params.database {
            Some(database) => match database {
                Database::ParityDb => {}
                Database::RocksDb | Database::Auto | Database::ParityDbDeprecated => {
                    result.invalid_database_backend = Err(database);
                    cli.run.import_params.database_params.database =
                        Some(Self::default_database_for_pruning());
                }
            },
            None => {
                cli.run.import_params.database_params.database =
                    Some(Self::default_database_for_pruning());
            }
        }
        result
    }

    pub fn report_pruning_validation_result(self) {
        if !self.pruning_enabled {
            if self.overwritten_pruning {
                warn!("Pruning not supported. Switching to keeping all block bodies and states.");
            }
            return;
        }
        if let Err(max_blocks) = self.invalid_state_pruning_setting {
            warn!(
                "State pruning was enabled but the `state_pruning` parameter
                is smaller than minimal supported value (min: {}). Further
                execution can lead to misbehaviour, which can be punished. State
                pruning: {};",
                Self::minimum_state_pruning_when_pruning_enabled(),
                max_blocks
            );
        }
        if let Err(blocks_pruning) = self.invalid_blocks_pruning_setting {
            warn!(
            "Blocks pruning was enabled but the provided value for the `blocks_pruning` parameter is invalid ({blocks_pruning}).
               Supported values are `Archive` and `ArchiveCanonical`.",
        );
        }
        if let Err(database) = self.invalid_database_backend {
            warn!(
                "State pruning was enabled but the selected database backend ({:?})
                is not supported while using pruning. Forced switch to `ParityDB`.",
                database
            );
        }
    }

    const fn default_state_pruning() -> DatabasePruningMode {
        DatabasePruningMode::Archive
    }

    const fn default_blocks_pruning() -> DatabasePruningMode {
        DatabasePruningMode::ArchiveCanonical
    }

    const fn minimum_state_pruning_when_pruning_enabled() -> u32 {
        // Anything greater than 1800, which is two normal lenght sessions (900 each), should be enough.
        // Every new session node needs to read state that contains a list of validators from two session before.
        2048
    }

    const fn default_database_for_pruning() -> sc_cli::Database {
        Database::ParityDb
    }

    fn pruning_changed(params: &PruningParams) -> bool {
        let state_pruning_changed = match params.state_pruning {
            Some(state_pruning) => state_pruning != Self::default_state_pruning(),
            None => false,
        };

        let blocks_pruning_changed = params.blocks_pruning != Self::default_blocks_pruning();

        state_pruning_changed || blocks_pruning_changed
    }
}

fn enforce_heap_pages(config: &mut Configuration) {
    config.default_heap_pages = Some(HEAP_PAGES);
}

fn main() -> sc_cli::Result<()> {
    let mut cli = Cli::parse();
    let pruning_config_validation_result =
        PruningConfigValidator::validate_and_fix_parameters(&mut cli);

    match &cli.subcommand {
        Some(Subcommand::BootstrapChain(cmd)) => cmd.run(),
        Some(Subcommand::BootstrapNode(cmd)) => cmd.run(),
        Some(Subcommand::ConvertChainspecToRaw(cmd)) => cmd.run(),
        Some(Subcommand::Key(cmd)) => cmd.run(&cli),
        Some(Subcommand::CheckBlock(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    import_queue,
                    ..
                } = new_partial(&config)?;
                Ok((cmd.run(client, import_queue), task_manager))
            })
        }
        Some(Subcommand::ExportBlocks(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    ..
                } = new_partial(&config)?;
                Ok((cmd.run(client, config.database), task_manager))
            })
        }
        Some(Subcommand::ExportState(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    ..
                } = new_partial(&config)?;
                Ok((cmd.run(client, config.chain_spec), task_manager))
            })
        }
        Some(Subcommand::ImportBlocks(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    import_queue,
                    ..
                } = new_partial(&config)?;
                Ok((cmd.run(client, import_queue), task_manager))
            })
        }
        Some(Subcommand::PurgeChain(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run(config.database))
        }
        Some(Subcommand::Revert(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    backend,
                    ..
                } = new_partial(&config)?;
                Ok((cmd.run(client, backend, None), task_manager))
            })
        }
        #[cfg(feature = "try-runtime")]
        Some(Subcommand::TryRuntime(cmd)) => {
            use primitives::MILLISECS_PER_BLOCK;
            use sc_executor::{sp_wasm_interface::ExtendedHostFunctions, NativeExecutionDispatch};
            use try_runtime_cli::block_building_info::timestamp_with_aura_info;
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let registry = config.prometheus_config.as_ref().map(|cfg| &cfg.registry);
                let task_manager =
                    sc_service::TaskManager::new(config.tokio_handle.clone(), registry)
                        .map_err(|e| sc_cli::Error::Service(sc_service::Error::Prometheus(e)))?;

                Ok((
                    cmd.run::<Block, ExtendedHostFunctions<
                        sp_io::SubstrateHostFunctions,
                        <ExecutorDispatch as NativeExecutionDispatch>::ExtendHostFunctions,
                    >, _>(Some(timestamp_with_aura_info(
                        MILLISECS_PER_BLOCK,
                    ))),
                    task_manager,
                ))
            })
        }
        #[cfg(not(feature = "try-runtime"))]
        Some(Subcommand::TryRuntime) => Err("TryRuntime wasn't enabled when building the node. \
        You can enable it with `--features try-runtime`."
            .into()),
        #[cfg(feature = "runtime-benchmarks")]
        Some(Subcommand::Benchmark(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| {
                if let frame_benchmarking_cli::BenchmarkCmd::Pallet(cmd) = cmd {
                    cmd.run::<Block, ()>(config)
                } else {
                    Err(sc_cli::Error::Input("Wrong subcommand".to_string()))
                }
            })
        }
        #[cfg(not(feature = "runtime-benchmarks"))]
        Some(Subcommand::Benchmark) => Err(
            "Benchmarking wasn't enabled when building the node. You can enable it with \
                    `--features runtime-benchmarks`."
                .into(),
        ),
        None => {
            let runner = cli.create_runner(&cli.run)?;

            pruning_config_validation_result.report_pruning_validation_result();

            let mut aleph_cli_config = cli.aleph;
            runner.run_node_until_exit(|mut config| async move {
                if matches!(config.role, Role::Full) {
                    if !aleph_cli_config.external_addresses().is_empty() {
                        panic!(
                            "A non-validator node cannot be run with external addresses specified."
                        );
                    }
                    // We ensure that external addresses for non-validator nodes are set, but to a
                    // value that is not routable. This will no longer be neccessary once we have
                    // proper support for non-validator nodes, but this requires a major
                    // refactor.
                    info!(
                        "Running as a non-validator node, setting dummy addressing configuration."
                    );
                    aleph_cli_config.set_dummy_external_addresses();
                }
                enforce_heap_pages(&mut config);
                new_authority(config, aleph_cli_config).map_err(sc_cli::Error::Service)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use sc_service::{BlocksPruning, PruningMode};

    use super::{PruningConfigValidator, PruningParams};

    #[test]
    fn pruning_sanity_check() {
        let pruning_params = PruningParams {
            state_pruning: Some(PruningConfigValidator::default_state_pruning()),
            blocks_pruning: PruningConfigValidator::default_blocks_pruning(),
        };

        assert_eq!(
            pruning_params.blocks_pruning().unwrap(),
            BlocksPruning::KeepFinalized
        );

        assert_eq!(
            pruning_params.state_pruning().unwrap().unwrap(),
            PruningMode::ArchiveAll
        );
    }
}
