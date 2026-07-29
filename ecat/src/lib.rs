mod hook;
mod signal;

pub use hook::LifecycleHook;
pub use signal::wait_for_shutdown;

use ecat_transport::Server;
use std::sync::Arc;

pub struct App {
    name: String,
    version: String,
    servers: Vec<Arc<dyn Server>>,
    lifecycle_hooks: Vec<Box<dyn LifecycleHook>>,
}

impl App {
    pub fn builder() -> AppBuilder {
        AppBuilder::default()
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        ecat_logging::init();

        tracing::info!(name = self.name, version = self.version, "starting application");

        for hook in &self.lifecycle_hooks {
            hook.on_start().await?;
        }

        for server in &self.servers {
            let server = Arc::clone(server);
            tokio::spawn(async move {
                if let Err(e) = server.start().await {
                    tracing::error!(error = %e, "server error");
                }
            });
        }

        wait_for_shutdown().await;

        tracing::info!("shutting down");
        for hook in &self.lifecycle_hooks {
            hook.on_stop().await?;
        }
        for server in &self.servers {
            if let Err(e) = server.stop().await {
                tracing::error!(error = %e, "server stop error");
            }
        }

        Ok(())
    }
}

#[derive(Default)]
pub struct AppBuilder {
    name: Option<String>,
    version: Option<String>,
    servers: Vec<Arc<dyn Server>>,
    lifecycle_hooks: Vec<Box<dyn LifecycleHook>>,
}

impl AppBuilder {
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn server(mut self, server: impl Server + 'static) -> Self {
        self.servers.push(Arc::new(server));
        self
    }

    pub fn on_start(mut self, hook: impl LifecycleHook + 'static) -> Self {
        self.lifecycle_hooks.push(Box::new(hook));
        self
    }

    pub fn on_stop(mut self, hook: impl LifecycleHook + 'static) -> Self {
        self.lifecycle_hooks.push(Box::new(hook));
        self
    }

    pub fn build(self) -> Result<App, Box<dyn std::error::Error + Send + Sync>> {
        Ok(App {
            name: self.name.unwrap_or_else(|| "ecat-app".into()),
            version: self.version.unwrap_or_else(|| "0.1.0".into()),
            servers: self.servers,
            lifecycle_hooks: self.lifecycle_hooks,
        })
    }
}
