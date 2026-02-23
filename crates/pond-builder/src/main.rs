use clap::{Parser, Subcommand};
use anyhow::Result;
use std::fs;
use heck::{ToPascalCase, ToSnakeCase};
use indoc::indoc;

#[derive(Parser)]
#[command(name = "pond-builder")]
#[command(about = "Automation tool for Goose-in-a-Pond Hexagonal Architecture", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new Port (Trait) in pond-core
    #[command(name = "make:port")]
    MakePort {
        /// Name of the port (e.g. Storage)
        name: String,
        /// Type of port: driving (input) or driven (output)
        #[arg(long, default_value = "driven")]
        port_type: String,
    },
    /// Generate a concrete implementation of an existing Port
    #[command(name = "make:adapter")]
    MakeAdapter {
        /// Name of the adapter (e.g. Sqlite)
        name: String,
        /// The port name it implements
        #[arg(long)]
        r#for: String,
        /// The target crate path (relative to root)
        #[arg(long)]
        target_crate: String,
    },
    /// Generate an orchestrator service in pond-core
    #[command(name = "make:service")]
    MakeService {
        /// Name of the service
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::MakePort { name, port_type } => {
            println!("Building {} port: {}...", port_type, name);
            generate_port(&name, &port_type)?;
        }
        Commands::MakeAdapter { name, r#for, target_crate } => {
            println!("Building adapter {} for port {} in {}...", name, r#for, target_crate);
            generate_adapter(&name, &r#for, &target_crate)?;
        }
        Commands::MakeService { name } => {
            println!("Building service: {}...", name);
            generate_service(&name)?;
        }
    }

    Ok(())
}

fn generate_port(name: &str, port_type: &str) -> Result<()> {
    let trait_name = name.to_pascal_case();
    let file_name = name.to_snake_case();
    let port_dir = "crates/pond-core/src/ports";
    fs::create_dir_all(port_dir)?;
    
    let port_path = format!("{}/{}.rs", port_dir, file_name);
    
    let content = format!(
        indoc! {r#"
            use anyhow::Result;
            use thiserror::Error;

            #[derive(Error, Debug)]
            pub enum {trait_name}Error {{
                #[error("General error: {{0}}")]
                General(String),
            }}

            /// {port_type} Port: {trait_name}
            /// 
            /// This trait defines the interface for {name}.
            pub trait {trait_name}: Send + Sync {{
                // Define your methods here
            }}
        "#},
        trait_name = trait_name,
        port_type = port_type.to_pascal_case(),
        name = name
    );

    fs::write(&port_path, content)?;
    println!("SUCCESS: Created port at {}", port_path);
    
    // Suggest registration info
    println!("\nNext steps:");
    println!("1. Add `pub mod {};` to crates/pond-core/src/ports/mod.rs", file_name);
    
    Ok(())
}

fn generate_adapter(name: &str, r#for: &str, target_crate: &str) -> Result<()> {
    let port_name = r#for.to_pascal_case();
    let struct_name = format!("{}{}", name.to_pascal_case(), port_name);
    let file_name = format!("{}_{}", name.to_snake_case(), port_name.to_snake_case());
    let adapter_path = format!("{}/src/{}.rs", target_crate, file_name);

    let content = format!(
        indoc! {r#"
            use pond_core::ports::{port_module}::{port_trait};
            use anyhow::Result;

            /// Adapter: {struct_name}
            /// 
            /// Implementation of the {port_trait} port.
            pub struct {struct_name} {{
                // Add dependencies (e.g. DB pool, config)
            }}

            impl {struct_name} {{
                pub fn new() -> Self {{
                    Self {{}}
                }}
            }}

            impl {port_trait} for {struct_name} {{
                // Implement trait methods here
            }}
        "#},
        port_module = port_name.to_snake_case(),
        port_trait = port_name,
        struct_name = struct_name
    );

    // Ensure the src directory exists
    let src_dir = format!("{}/src", target_crate);
    fs::create_dir_all(&src_dir)?;

    fs::write(&adapter_path, content)?;
    println!("SUCCESS: Created adapter at {}", adapter_path);
    
    Ok(())
}

fn generate_service(name: &str) -> Result<()> {
    let service_name = format!("{}Service", name.to_pascal_case());
    let file_name = name.to_snake_case();
    let service_dir = "crates/pond-core/src/services";
    fs::create_dir_all(service_dir)?;
    
    let service_path = format!("{}/{}.rs", service_dir, file_name);

    let content = format!(
        indoc! {r#"
            use anyhow::Result;
            
            /// Domain Service: {service_name}
            /// 
            /// This service orchestrates business logic using injected ports.
            pub struct {service_name} {{
                // Add ports here as Box<dyn PortName>
            }}

            impl {service_name} {{
                pub fn new() -> Self {{
                    Self {{}}
                }}

                pub async fn execute(&self) -> Result<()> {{
                    // Implement orchestration logic
                    Ok(())
                }}
            }}
        "#},
        service_name = service_name
    );

    fs::write(&service_path, content)?;
    println!("SUCCESS: Created service at {}", service_path);
    
    Ok(())
}
