//! cc-api - CLI tool for testing the cc-proxy API
//!
//! This tool provides a command-line interface to interact with all
//! cc-proxy API endpoints, useful for testing and debugging.

mod client;
mod poke;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use shared::api::{CcProxyApi, CreateProxyTokenRequest};
use tabled::{Table, Tabled};

use client::NativeApiClient;

#[derive(Parser)]
#[command(name = "cc-api")]
#[command(about = "CLI tool for testing cc-proxy API", long_about = None)]
struct Cli {
    /// Server URL
    #[arg(short, long, default_value = "http://localhost:3000")]
    server: String,

    /// Auth token (for authenticated endpoints)
    #[arg(short, long, env = "CC_PROXY_TOKEN")]
    token: Option<String>,

    /// Output format
    #[arg(short, long, default_value = "pretty")]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Default, clap::ValueEnum)]
enum OutputFormat {
    #[default]
    Pretty,
    Json,
    Table,
}

#[derive(Subcommand)]
enum Commands {
    /// Check server health
    Health,

    /// Get current user info
    Me,

    /// Session management
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },

    /// Proxy token management
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },

    /// Device flow authentication
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },

    /// Send a message to an active session (for debugging)
    Poke {
        /// Session ID (UUID)
        session_id: String,
        /// Message to send
        message: String,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// List all sessions
    List,
    /// Get a specific session
    Get {
        /// Session ID or key
        id: String,
    },
    /// Delete a session
    Delete {
        /// Session ID or key
        id: String,
    },
}

#[derive(Subcommand)]
enum TokenAction {
    /// Create a new proxy token
    Create {
        /// Optional session name prefix
        #[arg(short, long)]
        prefix: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Start device flow authentication
    Login,
    /// Check authentication status
    Status,
}

#[derive(Tabled)]
struct SessionRow {
    #[tabled(rename = "ID")]
    id: String,
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "Directory")]
    directory: String,
    #[tabled(rename = "Last Activity")]
    last_activity: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let client = NativeApiClient::new(&cli.server, cli.token.as_deref());

    match cli.command {
        Commands::Health => {
            let health = client.health().await?;
            match cli.format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&health)?),
                OutputFormat::Pretty | OutputFormat::Table => {
                    println!("{} Server is {}", "✓".green(), health.status.green());
                    if let Some(v) = health.version {
                        println!("  Version: {}", v);
                    }
                }
            }
        }

        Commands::Me => {
            let user = client.get_me().await?;
            match cli.format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&user)?),
                OutputFormat::Pretty | OutputFormat::Table => {
                    println!("{} {}", "User:".bold(), user.email);
                    if let Some(name) = user.name {
                        println!("  Name: {}", name);
                    }
                    println!("  ID: {}", user.id);
                }
            }
        }

        Commands::Sessions { action } => match action {
            SessionAction::List => {
                let sessions = client.list_sessions().await?;
                match cli.format {
                    OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&sessions)?),
                    OutputFormat::Table => {
                        if sessions.is_empty() {
                            println!("No sessions found");
                        } else {
                            let rows: Vec<SessionRow> = sessions
                                .iter()
                                .map(|s| SessionRow {
                                    id: s.id.to_string()[..8].to_string(),
                                    name: s.session_name.clone(),
                                    status: s.status.as_str().to_string(),
                                    directory: if s.working_directory.is_empty() {
                                        "-".to_string()
                                    } else {
                                        s.working_directory.clone()
                                    },
                                    last_activity: s.last_activity.clone(),
                                })
                                .collect();
                            let table = Table::new(rows);
                            println!("{}", table);
                        }
                    }
                    OutputFormat::Pretty => {
                        if sessions.is_empty() {
                            println!("No sessions found");
                        } else {
                            println!("{} {} session(s):", "Found".bold(), sessions.len());
                            for s in &sessions {
                                let status_color = match s.status {
                                    shared::SessionStatus::Active => "green",
                                    shared::SessionStatus::Inactive => "yellow",
                                    shared::SessionStatus::Disconnected => "red",
                                };
                                println!(
                                    "\n  {} {}",
                                    "●".color(status_color),
                                    s.session_name.bold()
                                );
                                println!("    ID: {}", s.id);
                                if !s.working_directory.is_empty() {
                                    println!("    Directory: {}", s.working_directory.cyan());
                                }
                                println!("    Last active: {}", s.last_activity);
                            }
                        }
                    }
                }
            }
            SessionAction::Get { id } => {
                let session = client.get_session(&id).await?;
                match cli.format {
                    OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&session)?),
                    OutputFormat::Pretty | OutputFormat::Table => {
                        println!("{} {}", "Session:".bold(), session.session_name);
                        println!("  ID: {}", session.id);
                        println!("  Status: {}", session.status.as_str());
                        if !session.working_directory.is_empty() {
                            println!("  Directory: {}", session.working_directory);
                        }
                        println!("  Last activity: {}", session.last_activity);
                    }
                }
            }
            SessionAction::Delete { id } => {
                client.delete_session(&id).await?;
                println!("{} Session {} deleted", "✓".green(), id);
            }
        },

        Commands::Token { action } => match action {
            TokenAction::Create { prefix } => {
                let req = CreateProxyTokenRequest {
                    session_name_prefix: prefix,
                };
                let resp = client.create_proxy_token(req).await?;
                match cli.format {
                    OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&resp)?),
                    OutputFormat::Pretty | OutputFormat::Table => {
                        println!("{} Proxy token created", "✓".green());
                        println!();
                        println!("{}:", "Setup Command".bold());
                        println!("  {}", resp.setup_command.cyan());
                        println!();
                        println!("{}:", "Setup URL".bold());
                        println!("  {}", resp.setup_url);
                        println!();
                        println!("Expires: {}", resp.expires_at);
                    }
                }
            }
        },

        Commands::Poke {
            session_id,
            message,
        } => {
            poke::poke_session(&cli.server, &session_id, &message).await?;
        }

        Commands::Auth { action } => match action {
            AuthAction::Login => {
                println!("{} Starting device flow authentication...", "→".blue());
                let code_resp = client.request_device_code().await?;

                println!();
                println!("{}:", "Verification Code".bold());
                println!("  {}", code_resp.user_code.green().bold());
                println!();
                println!("{}:", "Open this URL".bold());
                println!("  {}", code_resp.verification_uri.cyan());
                println!();
                println!(
                    "Waiting for authorization (expires in {}s)...",
                    code_resp.expires_in
                );

                // Poll for completion
                let interval = std::time::Duration::from_secs(code_resp.interval);
                loop {
                    tokio::time::sleep(interval).await;
                    print!(".");
                    std::io::Write::flush(&mut std::io::stdout())?;

                    match client.poll_device_code(&code_resp.device_code).await? {
                        shared::DevicePollResponse::Pending => continue,
                        shared::DevicePollResponse::Complete {
                            access_token,
                            user_id: _,
                            user_email,
                        } => {
                            println!();
                            println!("{} Authenticated as {}", "✓".green(), user_email.bold());
                            println!();
                            println!("{}:", "Token".bold());
                            println!("  {}", access_token);
                            println!();
                            println!("Set this token with:");
                            println!("  export CC_PROXY_TOKEN=\"{}\"", access_token);
                            break;
                        }
                        shared::DevicePollResponse::Expired => {
                            println!();
                            println!("{} Device code expired", "✗".red());
                            break;
                        }
                        shared::DevicePollResponse::Denied => {
                            println!();
                            println!("{} Authorization denied", "✗".red());
                            break;
                        }
                    }
                }
            }
            AuthAction::Status => match client.get_me().await {
                Ok(user) => {
                    println!("{} Authenticated as {}", "✓".green(), user.email.bold());
                }
                Err(e) => {
                    println!("{} Not authenticated: {}", "✗".red(), e);
                }
            },
        },
    }

    Ok(())
}
