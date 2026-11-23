use runbook_server::{
    Command, Error, bootstrap, config::ServerConfig, parse_admin_command, parse_command,
};
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string().map_err(|_| {
                Error("usage: runbook-server <bootstrap|doctor|migrate|copy-blobs|serve|admin|assign-owner|export|import|--version>")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let command = if args.first().is_some_and(|arg| arg == "admin") {
        match parse_admin_command(args.into_iter().skip(1)) {
            Ok(command) => command,
            Err(error) => error.exit(),
        }
    } else {
        parse_command(args)?
    };
    if command == Command::Version {
        println!("{}", runbook_server::version_line());
        return Ok(());
    }
    if command == Command::Migrate {
        // A migration Job only needs database settings; it must not depend
        // on the public origin or any other serving configuration.
        let database =
            runbook_store_postgres::DatabaseConfig::from_env().map_err(|error| Error(error.0))?;
        let version = bootstrap::migrate(&database).await?;
        println!("Postgres schema {version} is current.");
        return Ok(());
    }
    if let Command::ResetMfa(email) = &command {
        let configured = std::env::var("RUNBOOK_AUTH_COMMAND").ok();
        let mut argv = configured
            .as_deref()
            .map_or_else(
                || runbook_server::auth_child::DEFAULT_COMMAND.to_vec(),
                |value| value.split_whitespace().collect(),
            )
            .into_iter();
        let executable = argv.next().ok_or(Error("config.invalid_auth_command"))?;
        let status = std::process::Command::new(executable)
            .args(argv)
            .args(["mfa-reset", email])
            .env_clear()
            .envs(runbook_server::auth_child::inherited_env(std::env::vars()))
            .envs(
                std::env::var("RUNBOOK_ORIGIN")
                    .ok()
                    .map(|origin| ("RUNBOOK_ORIGIN", origin)),
            )
            .status()
            .map_err(|_| Error("mfa.reset_command_unavailable"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(Error("mfa.reset_failed").into())
        };
    }
    let config = ServerConfig::from_env()?;
    match command {
        Command::ResetMfa(_) => unreachable!(),
        Command::AssignOwner { org, user } => {
            let result = runbook_server::owner_recovery::run(&config, &org, &user).await?;
            println!(
                "{}",
                match result {
                    runbook_store_postgres::OwnerAssignment::Assigned =>
                        "Owner assignment recorded.",
                    runbook_store_postgres::OwnerAssignment::AlreadyAssigned =>
                        "Owner assignment already recorded.",
                }
            );
            Ok(())
        }
        // HOST1: migrate, serve, and shut down gracefully on SIGTERM
        // (`runbook_server::lifecycle`).
        Command::Serve => Ok(runbook_server::lifecycle::run(config).await?),
        // Answered above, before serving configuration is read.
        Command::Migrate => Ok(()),
        Command::CopyBlobs => {
            let storage = config
                .blob_store
                .build_storage()
                .map_err(|_| runbook_server::Error("config.invalid_blob_store"))?
                .ok_or(runbook_server::Error("config.blob_store_required"))?;
            let copier = runbook_store_postgres::BlobCopier::new(config.database.clone());
            let stats = copier.copy_all(&storage, 64).await.map_err(|error| {
                runbook_server::Error(match error {
                    runbook_core::StoreError::NotFound => "copy_blobs.not_found",
                    runbook_core::StoreError::InvalidBatch(code)
                    | runbook_core::StoreError::Backend(code) => code,
                })
            })?;
            println!(
                "Copied {} blobs ({} bytes); {} raced another writer.",
                stats.copied, stats.bytes, stats.raced
            );
            Ok(())
        }
        Command::Bootstrap(args) => {
            let mut options = bootstrap::BootstrapOptions::default();
            if let Some(name) = args.owner_name {
                options.user_name = name.trim().to_owned();
            }
            // AUTH5: the headless bootstrap always issues tokens; by email,
            // only on request.
            options.with_token = args.owner_email.is_none() || args.with_token;
            options.owner_email = args.owner_email;
            Ok(bootstrap::run(&config, &options)
                .await?
                .write_once(std::io::stdout().lock())?)
        }
        // Answered above, before the configuration is read.
        Command::Version => Ok(()),
        Command::Doctor => {
            println!("{}", bootstrap::doctor(&config).await?);
            Ok(())
        }
        Command::Export { org, project, out } => {
            let created_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| Error("export.clock"))?
                .as_millis() as i64;
            let message = runbook_server::archive::export(
                &config.database,
                &org,
                &project,
                &out,
                created_at_ms,
            )
            .await?;
            println!("{message}");
            Ok(())
        }
        Command::Import {
            input,
            org,
            into_existing_org,
        } => {
            let options = runbook_core::portable::ImportOptions { into_existing_org };
            let message =
                runbook_server::archive::import(&config.database, &input, org.as_ref(), options)
                    .await?;
            println!("{message}");
            Ok(())
        }
    }
}
